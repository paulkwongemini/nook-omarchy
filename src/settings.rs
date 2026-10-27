use gtk::{gio, glib, prelude::*};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn parse(value: &str) -> Result<Self, Box<dyn std::error::Error>> {
        match value {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err("Unknown appearance setting".into()),
        }
    }

    pub fn apply(self) {
        if let Some(settings) = gtk::Settings::default() {
            // Reset overrides first so System resumes tracking GTK's desktop settings.
            settings.reset_property("gtk-theme-name");
            settings.reset_property("gtk-application-prefer-dark-theme");
            if self != Self::System {
                settings.set_gtk_theme_name(Some("Adwaita"));
                settings.set_gtk_application_prefer_dark_theme(self == Self::Dark);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub theme: Theme,
    pub chat: bool,
    pub gmail: bool,
    pub sound: bool,
    pub preview: bool,
    pub persistent: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            chat: true,
            gmail: true,
            sound: true,
            preview: true,
            persistent: true,
        }
    }
}

impl Preferences {
    pub fn path() -> PathBuf {
        glib::user_config_dir().join("nook-omarchy/settings.ini")
    }

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        let file = glib::KeyFile::new();
        file.load_from_data(&content, glib::KeyFileFlags::NONE)?;
        let mut result = Self::default();
        if file.has_key("Appearance", "Theme").unwrap_or(false) {
            result.theme = Theme::parse(file.string("Appearance", "Theme")?.as_str())?;
        }
        for (key, value) in [
            ("Chat", &mut result.chat),
            ("Gmail", &mut result.gmail),
            ("Sound", &mut result.sound),
            ("Preview", &mut result.preview),
            ("Persistent", &mut result.persistent),
        ] {
            if file.has_key("Notifications", key).unwrap_or(false) {
                *value = file.boolean("Notifications", key)?;
            }
        }
        Ok(result)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = glib::KeyFile::new();
        file.set_string("Appearance", "Theme", self.theme.id());
        for (key, value) in [
            ("Chat", self.chat),
            ("Gmail", self.gmail),
            ("Sound", self.sound),
            ("Preview", self.preview),
            ("Persistent", self.persistent),
        ] {
            file.set_boolean("Notifications", key, value);
        }
        gio::File::for_path(path).replace_contents(
            file.to_data().as_bytes(),
            None,
            false,
            gio::FileCreateFlags::PRIVATE | gio::FileCreateFlags::REPLACE_DESTINATION,
            gio::Cancellable::NONE,
        )?;
        Ok(())
    }

    pub fn allows(&self, service: usize) -> bool {
        match service {
            0 => self.chat,
            1 => self.gmail,
            _ => false,
        }
    }

    pub fn notification_parameters(
        &self,
        service: usize,
        title: &str,
        body: &str,
    ) -> glib::Variant {
        let mut hints = HashMap::new();
        hints.insert(
            "desktop-entry",
            "io.github.paulkwongemini.Nook".to_variant(),
        );
        hints.insert("suppress-sound", (!self.sound).to_variant());
        if self.sound {
            hints.insert("sound-name", "message-new-instant".to_variant());
        }
        // Omarchy's Quickshell caps normal toasts even with timeout=0. Critical
        // urgency is its supported no-timer path; it still respects Nook's DND.
        hints.insert(
            "urgency",
            (if self.persistent { 2u8 } else { 1u8 }).to_variant(),
        );
        let (title, body) = if self.preview {
            (title, body)
        } else {
            (
                if service == 1 { "Gmail" } else { "Chat" },
                "New notification",
            )
        };
        (
            "Nook",
            0u32,
            "nook-omarchy",
            title,
            body,
            vec!["default", "Open Nook"],
            hints,
            if self.persistent { 0i32 } else { -1i32 },
        )
            .to_variant()
    }
}

pub fn show_dialog(
    parent: &gtk::ApplicationWindow,
    current: &Preferences,
    save: impl Fn(Preferences) -> Result<(), Box<dyn std::error::Error>> + 'static,
    test: impl Fn() + 'static,
) {
    let dialog = gtk::Dialog::with_buttons(
        Some("Settings"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Save", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_default_width(440);
    dialog.set_resizable(false);
    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    let heading = |text: &str| {
        let label = gtk::Label::new(None);
        label.set_markup(&format!("<b>{text}</b>"));
        label.set_xalign(0.0);
        content.add(&label);
    };
    heading("Appearance");
    let theme = gtk::ComboBoxText::new();
    for (id, label) in [("system", "System"), ("light", "Light"), ("dark", "Dark")] {
        theme.append(Some(id), label);
    }
    theme.set_active_id(Some(current.theme.id()));
    if let Some(accessible) = theme.accessible() {
        accessible.set_name("App theme");
    }
    content.add(&theme);
    let note = gtk::Label::new(Some(
        "Changes Nook's sidebar and dialogs. Websites have their own theme settings.",
    ));
    note.set_line_wrap(true);
    note.set_max_width_chars(48);
    note.set_xalign(0.0);
    content.add(&note);
    heading("Notifications");
    let toggle = |label: &str, value: bool| {
        let button = gtk::CheckButton::with_label(label);
        button.set_active(value);
        content.add(&button);
        button
    };
    let chat = toggle("Chat notifications", current.chat);
    let gmail = toggle("Gmail notifications", current.gmail);
    let sound = toggle("Notification sound", current.sound);
    let preview = toggle("Show sender and message preview", current.preview);
    let persistent = toggle(
        "Keep notifications until clicked or dismissed",
        current.persistent,
    );
    let note = gtk::Label::new(Some("Persistent alerts use high priority on Omarchy. Nook must stay open; websites can withdraw alerts. Sound depends on your desktop. Changes apply to new alerts. Enable notifications in Chat or Gmail too."));
    note.set_line_wrap(true);
    note.set_max_width_chars(48);
    note.set_xalign(0.0);
    content.add(&note);
    let test_button = gtk::Button::with_label("Test saved notification settings");
    test_button.connect_clicked(move |_| test());
    content.add(&test_button);
    let error = gtk::Label::new(None);
    error.set_line_wrap(true);
    error.set_max_width_chars(48);
    error.set_xalign(0.0);
    content.add(&error);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let value = Preferences {
                theme: Theme::parse(theme.active_id().as_deref().unwrap_or("system")).unwrap_or_default(),
                chat: chat.is_active(), gmail: gmail.is_active(), sound: sound.is_active(),
                preview: preview.is_active(), persistent: persistent.is_active(),
            };
            if save(value).is_err() {
                error.set_text("Could not save settings. Check the configuration folder permissions and available disk space.");
                return;
            }
        }
        dialog.close();
    });
    dialog.show_all();
}
