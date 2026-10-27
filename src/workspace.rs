use crate::{
    notifications::Notifications,
    settings::{self, Preferences},
    policy::{self, Route, SERVICES},
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
use webkit2gtk::{
    CookieManagerExt, CookiePersistentStorage, DownloadExt, LoadEvent, NavigationPolicyDecisionExt,
    PermissionRequestExt, PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecisionExt,
    SettingsExt, URIRequestExt, URIResponseExt, WebContextExt, WebViewExt, WebsiteDataManagerExt,
};
use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

const AUTH_TIMEOUT: Duration = Duration::from_secs(300);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

struct Page {
    _wry: wry::WebView,
    web: webkit2gtk::WebView,
    service: usize,
    auth_until: Cell<Option<Instant>>,
    initial_popup: Cell<bool>,
    source: RefCell<Option<(String, Instant)>>,
    routed_away: Cell<bool>,
    load_failed: Cell<bool>,
    routed_uri: RefCell<String>,
    document_uri: RefCell<String>,
    container: gtk::Box,
    opener: Option<Weak<Page>>,
    ready: Cell<bool>,
}

impl Page {
    fn authenticating(&self) -> bool {
        self.auth_until
            .get()
            .is_some_and(|until| Instant::now() < until)
    }

    fn source(&self) -> String {
        let uri = self.document_uri.borrow();
        if policy::trusted_service(&uri) {
            return uri.to_string();
        }
        self.source
            .borrow()
            .as_ref()
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(source, _)| source.clone())
            .unwrap_or_default()
    }
}

pub struct Workspace {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    service_stacks: [gtk::Stack; 4],
    errors: [gtk::Label; 4],
    buttons: [gtk::ToggleButton; 4],
    pages: RefCell<[Option<Rc<Page>>; 4]>,
    popups: RefCell<Vec<Rc<Page>>>,
    downloads: RefCell<Vec<webkit2gtk::Download>>,
    selected: Cell<usize>,
    context: webkit2gtk::WebContext,
    anchor: webkit2gtk::WebView,
    notifications: Rc<Notifications>,
    preferences: RefCell<Preferences>,
    closing: Cell<bool>,
}

impl Workspace {
    pub fn new(
        app: &gtk::Application,
        data: &Path,
        cache: &Path,
    ) -> Result<Rc<Self>, Box<dyn std::error::Error>> {
        let loaded = Preferences::load(&Preferences::path());
        let settings_failed = loaded.is_err();
        let preferences = loaded.unwrap_or_else(|_| Preferences {
            chat: false, gmail: false, preview: false, ..Preferences::default()
        });
        preferences.theme.apply();
        let manager = webkit2gtk::WebsiteDataManager::builder()
            .base_data_directory(data.to_string_lossy())
            .base_cache_directory(cache.to_string_lossy())
            .build();
        if let Some(cookies) = manager.cookie_manager() {
            cookies.set_persistent_storage(
                &data.join("cookies.sqlite").to_string_lossy(),
                CookiePersistentStorage::Sqlite,
            );
        }
        let context = webkit2gtk::WebContext::with_website_data_manager(&manager);
        context.connect_initialize_notification_permissions(|context| {
            let allowed = [
                webkit2gtk::SecurityOrigin::new("https", "chat.google.com", 443),
                webkit2gtk::SecurityOrigin::new("https", "mail.google.com", 443),
            ];
            context.initialize_notification_permissions(&[&allowed[0], &allowed[1]], &[]);
        });
        // Wry's related-view API lets all views use this context's separate data/cache paths.
        let anchor = webkit2gtk::WebView::with_context(&context);
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Nook")
            .default_width(1200)
            .default_height(820)
            .build();
        window.connect_realize(|widget| {
            let Some(window) = widget.window() else { return };
            if window.display().type_().name() != "GdkWaylandDisplay" {
                return;
            }
            // GTK3 chooses SHM or GL at begin_paint. If WebKit first creates the
            // paint GL context during that paint, NVIDIA EGL enables explicit
            // sync but GTK still commits SHM without an acquire point. Prepare
            // the window-owned paint context before the first frame instead.
            // The returned, separate application context is not needed.
            if let Err(error) = window.create_gl_context() {
                eprintln!("Could not prepare Wayland GL rendering: {error}");
            }
        });
        window.set_size_request(420, 360);
        if let Some(icon) = svg_icon(include_str!("../assets/nook-omarchy.svg"), None, 64) {
            window.set_icon(Some(&icon));
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let rail = gtk::Box::new(gtk::Orientation::Vertical, 10);
        rail.set_width_request(60);
        rail.set_margin_top(16);
        rail.set_margin_bottom(12);
        rail.set_margin_start(8);
        rail.set_margin_end(8);
        let stack = gtk::Stack::new();
        let boxes: [gtk::Box; 4] = std::array::from_fn(|i| {
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
            stack.add_named(&box_, SERVICES[i].name);
            box_
        });
        let errors = std::array::from_fn(|i| {
            let label = gtk::Label::new(None);
            label.set_line_wrap(true);
            label.set_xalign(0.0);
            label.set_margin_start(12);
            label.set_margin_end(12);
            label.set_margin_top(10);
            label.set_margin_bottom(10);
            label.set_no_show_all(true);
            boxes[i].pack_start(&label, false, false, 0);
            label
        });
        let service_stacks = std::array::from_fn(|i| {
            let stack = gtk::Stack::new();
            boxes[i].pack_start(&stack, true, true, 0);
            stack
        });
        let buttons = std::array::from_fn(|i| {
            let button = gtk::ToggleButton::new();
            button.set_relief(gtk::ReliefStyle::None);
            button.set_size_request(44, 44);
            button.set_tooltip_text(Some(&format!("{} (Ctrl+{})", SERVICES[i].name, i + 1)));
            if let Some(accessible) = button.accessible() {
                accessible.set_name(SERVICES[i].name);
            }
            button.connect_style_updated(move |button| update_icon(button, i));
            update_icon(&button, i);
            rail.pack_start(&button, false, false, 0);
            button
        });
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rail.pack_start(&spacer, true, true, 0);
        let menu_button = gtk::MenuButton::new();
        menu_button.set_relief(gtk::ReliefStyle::None);
        menu_button.set_label("⋯");
        menu_button.set_size_request(44, 40);
        menu_button.set_tooltip_text(Some("Navigation and app options"));
        if let Some(accessible) = menu_button.accessible() {
            accessible.set_name("Nook menu");
        }
        rail.pack_end(&menu_button, false, false, 0);
        row.pack_start(&rail, false, false, 0);
        row.pack_start(&stack, true, true, 0);
        window.add(&row);
        let workspace = Rc::new_cyclic(|weak: &Weak<Self>| {
            let activate = weak.clone();
            let error = weak.clone();
            Self {
                window,
                stack,
                service_stacks,
                errors,
                buttons,
                pages: RefCell::new(std::array::from_fn(|_| None)),
                popups: RefCell::new(Vec::new()),
                downloads: RefCell::new(Vec::new()),
                selected: Cell::new(0),
                context,
                anchor,
                notifications: Notifications::new(
                    preferences.clone(),
                    move |service| {
                        if let Some(workspace) = activate.upgrade() {
                            workspace.select_service(service);
                            workspace.activate();
                        }
                    },
                    move |message| {
                        if let Some(workspace) = error.upgrade() {
                            workspace.failure(workspace.selected.get(), message);
                        }
                    },
                ),
                preferences: RefCell::new(preferences),
                closing: Cell::new(false),
            }
        });
        for i in 0..4 {
            let weak = Rc::downgrade(&workspace);
            workspace.buttons[i].connect_clicked(move |button| {
                if let Some(workspace) = weak.upgrade() {
                    if button.is_active() && workspace.selected.get() != i {
                        workspace.select_service(i);
                    } else if !button.is_active() && workspace.selected.get() == i {
                        button.set_active(true);
                    }
                }
            });
        }
        workspace.menu(&menu_button);
        if settings_failed {
            workspace.failure(0, "Could not read saved settings. Notifications and previews are disabled; open Settings to save new preferences.");
        }
        let weak = Rc::downgrade(&workspace);
        workspace.window.connect_key_press_event(move |_, event| {
            let handled = weak.upgrade().is_some_and(|workspace| workspace.key(event));
            if handled {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        let weak = Rc::downgrade(&workspace);
        workspace
            .context
            .connect_download_started(move |_, download| {
                if let Some(workspace) = weak.upgrade() {
                    workspace.download(download);
                } else {
                    download.cancel();
                }
            });
        let weak = Rc::downgrade(&workspace);
        workspace.window.connect_delete_event(move |window, _| {
            if let Some(workspace) = weak.upgrade() {
                workspace.shutdown();
            }
            if let Some(app) = window.application() {
                app.quit();
            }
            glib::Propagation::Proceed
        });
        workspace.window.show_all();
        Ok(workspace)
    }

    pub fn activate(&self) {
        self.window.present();
    }

    pub fn select_service(self: &Rc<Self>, service: usize) {
        if service >= SERVICES.len() || self.closing.get() {
            return;
        }
        let existing = self.pages.borrow()[service].clone();
        match existing {
            Some(page) => page,
            None => match self.make_page(service, None) {
                Ok(page) => {
                    self.pages.borrow_mut()[service] = Some(page.clone());
                    page.web.load_uri(SERVICES[service].url);
                    page
                }
                Err(_) => {
                    self.failure(
                        service,
                        "Could not create the web view. Restart Nook to try again.",
                    );
                    return;
                }
            },
        };
        self.selected.set(service);
        self.stack.set_visible_child_name(SERVICES[service].name);
        for (i, button) in self.buttons.iter().enumerate() {
            button.set_active(i == service);
        }
        self.present_service(service);
    }

    fn make_page(
        self: &Rc<Self>,
        service: usize,
        opener: Option<&Rc<Page>>,
    ) -> Result<Rc<Page>, wry::Error> {
        let related = opener
            .map(|page| page.web.clone())
            .unwrap_or_else(|| self.anchor.clone());
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let wry = WebViewBuilder::new()
            .with_related_view(related)
            .with_devtools(false)
            .build_gtk(&container)?;
        let web = wry.webview();
        if let Some(settings) = WebViewExt::settings(&web) {
            settings.set_enable_write_console_messages_to_stdout(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_allow_top_navigation_to_data_urls(false);
            settings.set_javascript_can_access_clipboard(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_webrtc(false);
        }
        let source = opener
            .map(|page| page.source())
            .filter(|source| policy::trusted_service(source))
            .map(|source| (source, Instant::now() + TRANSFER_TIMEOUT));
        let page = Rc::new(Page {
            _wry: wry,
            web,
            service,
            auth_until: Cell::new(opener.and_then(|page| page.auth_until.get())),
            initial_popup: Cell::new(opener.is_some()),
            source: RefCell::new(source),
            routed_away: Cell::new(false),
            load_failed: Cell::new(false),
            routed_uri: RefCell::new(String::new()),
            document_uri: RefCell::new(String::new()),
            container: container.clone(),
            opener: opener.map(Rc::downgrade),
            ready: Cell::new(opener.is_none()),
        });
        self.service_stacks[service].add(&container);
        self.connect_page(&page);
        container.show_all();
        Ok(page)
    }

    fn connect_page(self: &Rc<Self>, page: &Rc<Page>) {
        let weak = Rc::downgrade(self);
        let p = Rc::downgrade(page);
        page.web.connect_uri_notify(move |web| {
            if let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) {
                let uri = web.uri().unwrap_or_default();
                if !uri.is_empty() && !workspace.navigate(&page, &uri) {
                    web.stop_loading();
                }
            }
        });
        let weak = Rc::downgrade(self);
        let p = Rc::downgrade(page);
        page.web.connect_decide_policy(move |_, decision, kind| {
            let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) else {
                decision.ignore();
                return true;
            };
            match kind {
                PolicyDecisionType::NavigationAction => {
                    if let Some(nav) =
                        decision.downcast_ref::<webkit2gtk::NavigationPolicyDecision>()
                    {
                        let uri = nav
                            .navigation_action()
                            .and_then(|action| action.request())
                            .and_then(|request| request.uri())
                            .unwrap_or_default();
                        // This signal includes subframes. Top-level routing uses the active URI and response signals.
                        let scheme = url::Url::parse(&uri)
                            .ok()
                            .map(|url| url.scheme().to_owned());
                        if !matches!(scheme.as_deref(), Some("https" | "about" | "blob" | "data")) {
                            if policy::route(&uri, false, false) == Route::External {
                                workspace.open_external(&uri);
                            }
                            decision.ignore();
                            return true;
                        }
                    }
                    false
                }
                PolicyDecisionType::Response => {
                    if let Some(response) =
                        decision.downcast_ref::<webkit2gtk::ResponsePolicyDecision>()
                    {
                        if response.is_main_frame_main_resource() {
                            let uri = response
                                .response()
                                .and_then(|r| r.uri())
                                .unwrap_or_default();
                            if !workspace.navigate(&page, &uri) {
                                decision.ignore();
                                return true;
                            }
                            if !response.is_mime_type_supported() {
                                if policy::allows_download(&uri, &page.source()) {
                                    decision.download();
                                } else {
                                    decision.ignore();
                                }
                                return true;
                            }
                        }
                    }
                    false
                }
                _ => false,
            }
        });
        let weak = Rc::downgrade(self);
        let p = Rc::downgrade(page);
        page.web.connect_create(move |_, action| {
            let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) else {
                return None;
            };
            let uri = action
                .clone()
                .request()
                .and_then(|request| request.uri())
                .unwrap_or_default();
            workspace
                .popup(&page, &uri)
                .map(|page| page.web.clone().upcast())
        });
        let weak = Rc::downgrade(self);
        let p = Rc::downgrade(page);
        page.web.connect_load_changed(move |web, event| {
            let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) else {
                return;
            };
            if event == LoadEvent::Started {
                page.routed_away.set(false);
                page.routed_uri.borrow_mut().clear();
                page.load_failed.set(false);
                workspace.errors[page.service].hide();
            }
            if event == LoadEvent::Committed {
                let uri = web.uri().unwrap_or_default();
                if uri != "about:blank" {
                    page.source.borrow_mut().take();
                    page.initial_popup.set(false);
                }
                *page.document_uri.borrow_mut() = uri.to_string();
            }
            if event == LoadEvent::Finished
                && !page.load_failed.get()
                && policy::service_for_url(&web.uri().unwrap_or_default()).is_some()
            {
                page.auth_until.set(None);
            }
            if event == LoadEvent::Finished
                && !page.load_failed.get()
                && page.opener.is_some()
                && policy::service_for_url(&web.uri().unwrap_or_default()).is_some()
            {
                let pages: Vec<_> = workspace.pages.borrow().iter().flatten().cloned().collect();
                for original in pages {
                    let uri = original.web.uri().unwrap_or_default();
                    if policy::account_url(&uri) || uri == SERVICES[original.service].url {
                        original.web.load_uri(SERVICES[original.service].url);
                    }
                }
                let completed = workspace.clone();
                let service = page.service;
                glib::idle_add_local_once(move || {
                    let removed: Vec<_> = completed
                        .popups
                        .borrow()
                        .iter()
                        .filter(|page| page.service == service)
                        .cloned()
                        .collect();
                    completed.remove_popups(&removed);
                    completed.present_service(service);
                    completed.activate();
                });
            }
        });
        let weak = Rc::downgrade(self);
        let p = Rc::downgrade(page);
        page.web.connect_load_failed(move |_, _, _, error| {
            if let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) {
                page.load_failed.set(true);
                if !page.routed_away.get()
                    && !error.matches(webkit2gtk::NetworkError::Cancelled)
                    && !error.matches(webkit2gtk::PolicyError::FrameLoadInterruptedByPolicyChange)
                {
                    workspace.failure(
                        page.service,
                        "Could not load this page. Press Ctrl+R to retry or use Service Home.",
                    );
                }
            }
            true
        });
        let weak = Rc::downgrade(self);
        let service = page.service;
        page.web.connect_web_process_terminated(move |_, _| {
            if let Some(workspace) = weak.upgrade() {
                workspace.failure(service, "The web process stopped. Press Ctrl+R to retry.");
            }
        });
        page.web.connect_permission_request(move |_, request| {
            // Exact notification origins are pregranted by WebKit. Clipboard requests
            // omit their frame origin, so only native menu/keyboard paste is supported.
            request.deny();
            true
        });
        let notifications = self.notifications.clone();
        page.web
            .connect_show_notification(move |web, notification| {
                notifications.present(&web.uri().unwrap_or_default(), notification);
                true
            });
        if page.opener.is_some() {
            let weak = Rc::downgrade(self);
            let p = Rc::downgrade(page);
            page.web.connect_destroy(move |_| {
                if let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) {
                    workspace.close_popup(&page);
                }
            });
        }
        if page.opener.is_some() {
            let weak = Rc::downgrade(self);
            let p = Rc::downgrade(page);
            page.web.connect_ready_to_show(move |_| {
                if let (Some(workspace), Some(page)) = (weak.upgrade(), p.upgrade()) {
                    page.ready.set(true);
                    workspace.select_service(page.service);
                }
            });
        }
        let weak = Rc::downgrade(self);
        let service = page.service;
        page.web.connect_uri_notify(move |_| {
            if let Some(workspace) = weak.upgrade() {
                workspace.update_title(service);
            }
        });
    }

    fn navigate(self: &Rc<Self>, page: &Rc<Page>, uri: &str) -> bool {
        if *page.routed_uri.borrow() == uri {
            return false;
        }
        let source = page.source();
        if policy::attachment_candidate(uri, &source) {
            page.routed_away.set(true);
            *page.routed_uri.borrow_mut() = uri.to_owned();
            *page.source.borrow_mut() = Some((source, Instant::now() + TRANSFER_TIMEOUT));
            page.web.download_uri(uri);
            return false;
        }
        match policy::route(uri, page.authenticating(), page.initial_popup.get()) {
            Route::Internal => {
                page.routed_away.set(false);
                page.routed_uri.borrow_mut().clear();
                if policy::account_url(uri) && !page.authenticating() {
                    page.auth_until.set(Some(Instant::now() + AUTH_TIMEOUT));
                }
                true
            }
            Route::External => {
                page.routed_away.set(true);
                *page.routed_uri.borrow_mut() = uri.to_owned();
                self.open_external(uri);
                self.close_popup(page);
                false
            }
            Route::Block => {
                page.routed_away.set(true);
                *page.routed_uri.borrow_mut() = uri.to_owned();
                self.failure(page.service, "This link type is not supported.");
                false
            }
        }
    }

    fn popup(self: &Rc<Self>, opener: &Rc<Page>, uri: &str) -> Option<Rc<Page>> {
        let source = opener.source();
        if policy::attachment_candidate(uri, &source) {
            *opener.source.borrow_mut() = Some((source, Instant::now() + TRANSFER_TIMEOUT));
            opener.web.download_uri(uri);
            return None;
        }
        match policy::route(uri, opener.authenticating(), true) {
            Route::External => {
                self.open_external(uri);
                return None;
            }
            Route::Block if !uri.is_empty() => return None,
            _ => {}
        }
        let current = opener.web.uri().unwrap_or_default();
        if !policy::trusted_service(&current)
            && !policy::account_url(&current)
            && !opener.authenticating()
        {
            return None;
        }
        if self.popups.borrow().len() >= 8 {
            self.failure(
                opener.service,
                "Close a sign-in page before opening another.",
            );
            return None;
        }
        let page = match self.make_page(opener.service, Some(opener)) {
            Ok(page) => page,
            Err(_) => {
                self.failure(opener.service, "Could not open the sign-in page.");
                return None;
            }
        };
        self.popups.borrow_mut().push(page.clone());
        Some(page)
    }

    fn close_popup(self: &Rc<Self>, page: &Rc<Page>) {
        if page.opener.is_none() {
            return;
        }
        let workspace = self.clone();
        let page = page.clone();
        // WebKit may emit close/load signals from the view being removed.
        glib::idle_add_local_once(move || {
            let removed: Vec<_> = workspace
                .popups
                .borrow()
                .iter()
                .filter(|candidate| {
                    let mut ancestor = Some((*candidate).clone());
                    while let Some(current) = ancestor {
                        if Rc::ptr_eq(&current, &page) {
                            return true;
                        }
                        ancestor = current.opener.as_ref().and_then(Weak::upgrade);
                    }
                    false
                })
                .cloned()
                .collect();
            workspace.remove_popups(&removed);
            workspace.present_service(page.service);
        });
    }

    fn remove_popups(&self, removed: &[Rc<Page>]) {
        for page in removed {
            page.web.stop_loading();
            self.service_stacks[page.service].remove(&page.container);
        }
        self.popups
            .borrow_mut()
            .retain(|page| !removed.iter().any(|item| Rc::ptr_eq(item, page)));
    }

    fn page_for_service(&self, service: usize) -> Option<Rc<Page>> {
        self.popups
            .borrow()
            .iter()
            .rev()
            .find(|page| page.service == service && page.ready.get())
            .cloned()
            .or_else(|| self.pages.borrow()[service].clone())
    }

    fn present_service(&self, service: usize) {
        if let Some(page) = self.page_for_service(service) {
            self.service_stacks[service].set_visible_child(&page.container);
            if self.selected.get() == service {
                page.web.grab_focus();
                self.update_title(service);
            }
        }
    }

    fn update_title(&self, service: usize) {
        if self.selected.get() != service {
            return;
        }
        let suffix = self
            .page_for_service(service)
            .filter(|page| page.opener.is_some())
            .map(|page| {
                let host = page
                    .web
                    .uri()
                    .and_then(|uri| url::Url::parse(&uri).ok())
                    .and_then(|uri| uri.host_str().map(str::to_owned));
                format!(" — {}", host.as_deref().unwrap_or("Google Sign-In"))
            })
            .unwrap_or_default();
        self.window
            .set_title(&format!("Nook — {}{}", SERVICES[service].name, suffix));
    }

    fn open_external(&self, uri: &str) {
        if policy::route(uri, false, false) != Route::External {
            return;
        }
        if gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>).is_err() {
            self.failure(
                self.selected.get(),
                "Could not open the system handler for this link.",
            );
        }
    }

    fn failure(&self, service: usize, message: &str) {
        self.errors[service].set_text(message);
        self.errors[service].show();
    }

    fn current(&self) -> Option<Rc<Page>> {
        self.page_for_service(self.selected.get())
    }

    fn action(self: &Rc<Self>, name: &str) {
        match name {
            "Back" => {
                if let Some(page) = self.current() {
                    page.web.go_back();
                }
            }
            "Reload" => {
                if let Some(page) = self.current() {
                    page.routed_away.set(false);
                    page.web.reload();
                }
            }
            "Service Home" => {
                let workspace = self.clone();
                let service = self.selected.get();
                glib::idle_add_local_once(move || {
                    let removed: Vec<_> = workspace
                        .popups
                        .borrow()
                        .iter()
                        .filter(|page| page.service == service)
                        .cloned()
                        .collect();
                    workspace.remove_popups(&removed);
                    let page = workspace.pages.borrow()[service].clone();
                    if let Some(page) = page {
                        page.routed_away.set(false);
                        page.web.load_uri(SERVICES[service].url);
                    }
                    workspace.present_service(service);
                });
            }
            "Settings" => {
                let save_workspace = Rc::downgrade(self);
                let test_workspace = Rc::downgrade(self);
                settings::show_dialog(&self.window, &self.preferences.borrow(), move |preferences| {
                    preferences.save(&Preferences::path())?;
                    preferences.theme.apply();
                    if let Some(workspace) = save_workspace.upgrade() {
                        workspace.notifications.set_preferences(preferences.clone());
                        *workspace.preferences.borrow_mut() = preferences;
                    }
                    Ok(())
                }, move || {
                    if let Some(workspace) = test_workspace.upgrade() { workspace.notifications.send_test(); }
                });
            }
            "Test App Notification" => self.notifications.send_test(),
            "Notification Help" => {
                let dialog = gtk::MessageDialog::new(Some(&self.window), gtk::DialogFlags::MODAL, gtk::MessageType::Info,
                    gtk::ButtonsType::Close, "Enable desktop notifications in Chat settings. Nook delivers standard web notifications while running. A test banner does not verify incoming Chat messages. Notifications after Nook exits and service-worker Web Push are not supported or verified.");
                dialog.set_title("Notifications");
                dialog.run();
                dialog.close();
            }
            "Quit Nook" => self.window.close(),
            _ => {}
        }
    }

    fn menu(self: &Rc<Self>, button: &gtk::MenuButton) {
        let menu = gtk::Menu::new();
        for name in [
            "Back",
            "Reload",
            "Service Home",
            "",
            "Settings",
            "Test App Notification",
            "Notification Help",
            "",
            "Quit Nook",
        ] {
            if name.is_empty() {
                menu.append(&gtk::SeparatorMenuItem::new());
                continue;
            }
            let item = gtk::MenuItem::with_label(name);
            let weak = Rc::downgrade(self);
            item.connect_activate(move |_| {
                if let Some(workspace) = weak.upgrade() {
                    workspace.action(name);
                }
            });
            menu.append(&item);
        }
        menu.show_all();
        button.set_popup(Some(&menu));
    }

    fn key(self: &Rc<Self>, event: &gdk::EventKey) -> bool {
        let modifiers = event.state() & gtk::accelerator_get_default_mod_mask();
        let key = event.keyval();
        if modifiers == gdk::ModifierType::CONTROL_MASK {
            if key.to_unicode() == Some(',') {
                self.action("Settings");
                return true;
            }
            if let Some(c) = key.to_unicode() {
                if ('1'..='4').contains(&c) {
                    self.select_service((c as u8 - b'1') as usize);
                    return true;
                }
                if c.eq_ignore_ascii_case(&'r') {
                    self.action("Reload");
                    return true;
                }
                if c.eq_ignore_ascii_case(&'q') {
                    self.action("Quit Nook");
                    return true;
                }
            }
        }
        if modifiers == (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK)
            && key
                .to_unicode()
                .is_some_and(|c| c.eq_ignore_ascii_case(&'h'))
        {
            self.action("Service Home");
            return true;
        }
        if modifiers == gdk::ModifierType::MOD1_MASK && key == gdk::keys::constants::Left {
            self.action("Back");
            return true;
        }
        false
    }

    fn download(self: &Rc<Self>, download: &webkit2gtk::Download) {
        let Some(web) = download.web_view() else {
            download.cancel();
            return;
        };
        let pages: Vec<_> = self
            .pages
            .borrow()
            .iter()
            .flatten()
            .chain(self.popups.borrow().iter())
            .cloned()
            .collect();
        let Some(page) = pages.into_iter().find(|page| page.web == web) else {
            download.cancel();
            return;
        };
        let source = page.source();
        let uri = download
            .request()
            .and_then(|request| request.uri())
            .unwrap_or_default();
        if !policy::allows_download(&uri, &source) || self.downloads.borrow().len() >= 8 {
            download.cancel();
            return;
        }
        self.downloads.borrow_mut().push(download.clone());
        let weak = Rc::downgrade(self);
        let service = page.service;
        download.connect_decide_destination(move |download, suggested| {
            let Some(workspace) = weak.upgrade() else {
                download.cancel();
                return true;
            };
            let uri = download
                .response()
                .and_then(|response| response.uri())
                .or_else(|| download.request().and_then(|request| request.uri()))
                .unwrap_or_default();
            if workspace.closing.get() || !policy::allows_download(&uri, &source) {
                download.cancel();
                return true;
            }
            let chooser = gtk::FileChooserNative::new(
                Some("Save Download"),
                Some(&workspace.window),
                gtk::FileChooserAction::Save,
                Some("Save"),
                Some("Cancel"),
            );
            chooser.set_do_overwrite_confirmation(true);
            chooser.set_local_only(true);
            chooser.set_current_name(&policy::safe_filename(suggested));
            if let Some(dir) = glib::user_special_dir(glib::UserDirectory::Downloads) {
                chooser.set_current_folder(dir);
            }
            let accepted = chooser.run() == gtk::ResponseType::Accept;
            let destination = accepted.then(|| chooser.filename()).flatten();
            chooser.destroy();
            if workspace.closing.get() {
                download.cancel();
                return true;
            }
            if let Some(path) = destination {
                if let Ok(uri) = url::Url::from_file_path(path) {
                    download.set_allow_overwrite(true);
                    download.set_destination(uri.as_str());
                    workspace.download_progress(download, service);
                } else {
                    download.cancel();
                }
            } else {
                download.cancel();
            }
            true
        });
        let weak = Rc::downgrade(self);
        download.connect_failed(move |_, error| {
            if !error.matches(webkit2gtk::DownloadError::CancelledByUser) {
                if let Some(workspace) = weak.upgrade() {
                    workspace.failure(
                        service,
                        "Download failed. Retry the download from the service.",
                    );
                }
            }
        });
        let weak = Rc::downgrade(self);
        download.connect_finished(move |download| {
            if let Some(workspace) = weak.upgrade() {
                workspace
                    .downloads
                    .borrow_mut()
                    .retain(|item| item != download);
            }
        });
    }

    fn download_progress(&self, download: &webkit2gtk::Download, _service: usize) {
        let dialog = gtk::Dialog::with_buttons(
            Some("Downloading — Nook"),
            Some(&self.window),
            gtk::DialogFlags::DESTROY_WITH_PARENT,
            &[("Cancel", gtk::ResponseType::Cancel)],
        );
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_text(Some("Downloading…"));
        progress.set_margin_start(16);
        progress.set_margin_end(16);
        progress.set_margin_top(16);
        progress.set_margin_bottom(16);
        dialog.content_area().add(&progress);
        dialog.set_default_size(360, 100);
        let pending = download.downgrade();
        dialog.connect_response(move |dialog, _| {
            if let Some(download) = pending.upgrade() {
                download.cancel();
            }
            dialog.close();
        });
        let bar = progress.downgrade();
        download.connect_estimated_progress_notify(move |download| {
            if let Some(bar) = bar.upgrade() {
                bar.set_fraction(download.estimated_progress().clamp(0.0, 1.0));
            }
        });
        let window = dialog.downgrade();
        download.connect_finished(move |_| {
            if let Some(dialog) = window.upgrade() {
                dialog.close();
            }
        });
        dialog.show_all();
    }

    pub fn shutdown(&self) {
        if self.closing.replace(true) {
            return;
        }
        self.notifications.shutdown();
        let downloads = self.downloads.borrow().clone();
        for download in downloads {
            download.cancel();
        }
        for popup in self.popups.borrow().iter() {
            popup.web.stop_loading();
        }
        for page in self.pages.borrow().iter().flatten() {
            page.web.stop_loading();
        }
    }
}

fn update_icon(button: &gtk::ToggleButton, service: usize) {
    let svg = match SERVICES[service].icon {
        "chat" => include_str!("../assets/chat.svg"),
        "gmail" => include_str!("../assets/gmail.svg"),
        "drive" => include_str!("../assets/drive.svg"),
        _ => include_str!("../assets/calendar.svg"),
    };
    let color = button.style_context().color(gtk::StateFlags::NORMAL);
    if let Some(icon) = svg_icon(
        svg,
        Some(&format!(
            "#{:02x}{:02x}{:02x}",
            (color.red() * 255.0) as u8,
            (color.green() * 255.0) as u8,
            (color.blue() * 255.0) as u8
        )),
        24,
    ) {
        button.set_image(Some(&gtk::Image::from_pixbuf(Some(&icon))));
        button.set_always_show_image(true);
    }
}

fn svg_icon(svg: &str, color: Option<&str>, size: i32) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let loader = gtk::gdk_pixbuf::PixbufLoader::with_type("svg").ok()?;
    loader.set_size(size, size);
    let svg = color
        .map(|color| svg.replace("#000", color))
        .unwrap_or_else(|| svg.to_owned());
    loader.write(svg.as_bytes()).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}
