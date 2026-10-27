use crate::{policy, settings::Preferences};
use glib::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use webkit2gtk::NotificationExt;

const MAX_NOTIFICATIONS: usize = 128;
const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

struct Entry {
    notification: Option<webkit2gtk::Notification>,
    closed_handler: RefCell<Option<glib::SignalHandlerId>>,
    origin: String,
    tag: String,
    title: String,
    body: String,
    service: usize,
    desktop_id: Cell<u32>,
    submitted: Cell<bool>,
    cancelled: Cell<bool>,
}

pub struct Notifications {
    connection: RefCell<Option<gio::DBusConnection>>,
    subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
    entries: RefCell<Vec<Rc<Entry>>>,
    pending: Cell<usize>,
    stopped: Cell<bool>,
    preferences: RefCell<Preferences>,
    failed: Cell<bool>,
    activate: Box<dyn Fn(usize)>,
    error: Box<dyn Fn(&str)>,
}

fn escaped(text: &str, limit: usize) -> String {
    glib::markup_escape_text(&text.chars().take(limit).collect::<String>()).into()
}

fn close_native(connection: &gio::DBusConnection, id: u32) {
    if id != 0 {
        connection.call(
            Some(SERVICE),
            PATH,
            SERVICE,
            "CloseNotification",
            Some(&(id,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}

impl Notifications {
    pub fn new(
        preferences: Preferences,
        activate: impl Fn(usize) + 'static,
        error: impl Fn(&str) + 'static,
    ) -> Rc<Self> {
        let owner = Rc::new(Self {
            connection: RefCell::new(None),
            subscriptions: RefCell::new(Vec::new()),
            entries: RefCell::new(Vec::new()),
            pending: Cell::new(0),
            stopped: Cell::new(false),
            preferences: RefCell::new(preferences),
            failed: Cell::new(false),
            activate: Box::new(activate),
            error: Box::new(error),
        });
        let weak = Rc::downgrade(&owner);
        gio::bus_get(
            gio::BusType::Session,
            gio::Cancellable::NONE,
            move |result| {
                let Some(owner) = weak.upgrade().filter(|owner| !owner.stopped.get()) else {
                    return;
                };
                match result {
                    Ok(connection) => {
                        owner.subscribe(&connection);
                        *owner.connection.borrow_mut() = Some(connection);
                        let entries = owner.entries.borrow().clone();
                        for entry in entries {
                            owner.submit(entry);
                        }
                    }
                    Err(_) => {
                        owner.failed.set(true);
                        let entries = owner.entries.borrow().clone();
                        for entry in entries {
                            owner.finish(&entry, true);
                        }
                        (owner.error)("Desktop notification delivery failed.");
                    }
                }
            },
        );
        owner
    }

    pub fn present(self: &Rc<Self>, origin: &str, notification: &webkit2gtk::Notification) {
        let service = policy::notification_service(origin);
        if self.stopped.get()
            || !service.is_some_and(|service| self.preferences.borrow().allows(service))
            || self.pending.get() >= MAX_NOTIFICATIONS
        {
            notification.close();
            return;
        }
        let tag = notification.tag().unwrap_or_default().to_string();
        if !tag.is_empty() {
            let previous = self
                .entries
                .borrow()
                .iter()
                .find(|entry| {
                    entry.notification.is_some() && entry.origin == origin && entry.tag == tag
                })
                .cloned();
            if let Some(previous) = previous {
                self.finish(&previous, true);
            }
        }
        let entry = Rc::new(Entry {
            notification: Some(notification.clone()),
            closed_handler: RefCell::new(None),
            origin: origin.to_owned(),
            tag,
            title: escaped(notification.title().as_deref().unwrap_or(""), 256),
            body: escaped(notification.body().as_deref().unwrap_or(""), 4096),
            service: service.unwrap(),
            desktop_id: Cell::new(0),
            submitted: Cell::new(false),
            cancelled: Cell::new(false),
        });
        let weak_owner = Rc::downgrade(self);
        let weak_entry = Rc::downgrade(&entry);
        *entry.closed_handler.borrow_mut() = Some(notification.connect_closed(move |_| {
            if let (Some(owner), Some(entry)) = (weak_owner.upgrade(), weak_entry.upgrade()) {
                owner.finish(&entry, false);
            }
        }));
        self.enqueue(entry);
    }

    pub fn send_test(self: &Rc<Self>) {
        if self.stopped.get() {
            return;
        }
        if self.pending.get() >= MAX_NOTIFICATIONS {
            (self.error)("Too many pending desktop notifications.");
            return;
        }
        self.enqueue(Rc::new(Entry {
            notification: None,
            closed_handler: RefCell::new(None),
            origin: String::new(),
            tag: String::new(),
            title: "Nook notification test".into(),
            body: "This is a synthetic test notification. Click to open Chat.".into(),
            service: 0,
            desktop_id: Cell::new(0),
            submitted: Cell::new(false),
            cancelled: Cell::new(false),
        }));
    }

    fn enqueue(self: &Rc<Self>, entry: Rc<Entry>) {
        let oldest = {
            let entries = self.entries.borrow();
            entries
                .first()
                .filter(|_| entries.len() >= MAX_NOTIFICATIONS)
                .cloned()
        };
        if let Some(oldest) = oldest {
            self.finish(&oldest, true);
        }
        self.entries.borrow_mut().push(entry.clone());
        self.pending.set(self.pending.get() + 1);
        if self.failed.get() {
            self.finish(&entry, true);
            (self.error)("Desktop notification delivery failed.");
        } else if self.connection.borrow().is_some() {
            self.submit(entry);
        }
    }

    fn submit(self: &Rc<Self>, entry: Rc<Entry>) {
        if entry.cancelled.get() || entry.submitted.replace(true) {
            return;
        }
        let Some(connection) = self.connection.borrow().clone() else {
            return;
        };
        let parameters = self.preferences.borrow().notification_parameters(
            entry.service,
            &entry.title,
            &entry.body,
        );
        let weak = Rc::downgrade(self);
        let callback_connection = connection.clone();
        // Keep the connection and entry alive until the reply so cancellation or
        // shutdown can withdraw a notification whose ID arrives late.
        connection.call(
            Some(SERVICE),
            PATH,
            SERVICE,
            "Notify",
            Some(&parameters),
            Some(glib::VariantTy::new("(u)").unwrap()),
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            move |result| {
                let id = result
                    .ok()
                    .and_then(|reply| reply.get::<(u32,)>())
                    .map(|reply| reply.0)
                    .unwrap_or(0);
                let owner = weak.upgrade();
                if let Some(owner) = &owner {
                    owner.pending.set(owner.pending.get() - 1);
                }
                if entry.cancelled.get() || owner.as_ref().is_none_or(|owner| owner.stopped.get()) {
                    close_native(&callback_connection, id);
                } else if let Some(owner) = owner {
                    if id == 0 {
                        owner.finish(&entry, true);
                        (owner.error)("Desktop notification delivery failed.");
                    } else {
                        entry.desktop_id.set(id);
                    }
                }
            },
        );
    }

    fn subscribe(self: &Rc<Self>, connection: &gio::DBusConnection) {
        for member in ["ActionInvoked", "NotificationClosed"] {
            let weak = Rc::downgrade(self);
            let subscription = connection.signal_subscribe(
                Some(SERVICE),
                Some(SERVICE),
                Some(member),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, parameters| {
                    let Some(owner) = weak.upgrade().filter(|owner| !owner.stopped.get()) else {
                        return;
                    };
                    if member == "ActionInvoked" {
                        if let Some((id, action)) = parameters.get::<(u32, String)>() {
                            if action == "default" {
                                if let Some(entry) = owner.find(id) {
                                    (owner.activate)(entry.service);
                                    if !entry.cancelled.get() {
                                        if let Some(notification) = &entry.notification {
                                            notification.clicked();
                                        }
                                    }
                                    owner.finish(&entry, true);
                                }
                            }
                        }
                    } else if let Some((id, _)) = parameters.get::<(u32, u32)>() {
                        if let Some(entry) = owner.find(id) {
                            entry.desktop_id.set(0);
                            owner.finish(&entry, true);
                        }
                    }
                },
            );
            self.subscriptions.borrow_mut().push(subscription);
        }
    }

    fn find(&self, id: u32) -> Option<Rc<Entry>> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| id != 0 && entry.desktop_id.get() == id)
            .cloned()
    }

    fn finish(&self, entry: &Rc<Entry>, notify_engine: bool) {
        if entry.cancelled.replace(true) {
            return;
        }
        self.entries
            .borrow_mut()
            .retain(|item| !Rc::ptr_eq(item, entry));
        if !entry.submitted.get() {
            self.pending.set(self.pending.get() - 1);
        }
        if let Some(connection) = self.connection.borrow().as_ref() {
            close_native(connection, entry.desktop_id.replace(0));
        }
        if let Some(notification) = &entry.notification {
            if let Some(handler) = entry.closed_handler.borrow_mut().take() {
                notification.disconnect(handler);
            }
            if notify_engine {
                notification.close();
            }
        }
    }

    pub fn set_preferences(&self, preferences: Preferences) {
        let previous = self.preferences.replace(preferences.clone());
        // Withdraw previews already on screen when privacy is enabled, and any
        // notifications from a service that has just been muted (including queued ones).
        let entries = self.entries.borrow().clone();
        for entry in entries {
            if (previous.preview && !preferences.preview)
                || (entry.notification.is_some() && !preferences.allows(entry.service))
            {
                self.finish(&entry, true);
            }
        }
    }

    pub fn shutdown(&self) {
        if self.stopped.replace(true) {
            return;
        }
        let entries = self.entries.borrow().clone();
        for entry in entries {
            self.finish(&entry, true);
        }
        if let Some(connection) = self.connection.borrow().as_ref() {
            for subscription in self.subscriptions.borrow_mut().drain(..) {
                connection.signal_unsubscribe(subscription);
            }
        }
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        self.shutdown();
    }
}
