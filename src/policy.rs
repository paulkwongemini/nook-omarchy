use percent_encoding::percent_decode_str;
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Internal,
    External,
    Block,
}
pub struct Service {
    pub name: &'static str,
    pub url: &'static str,
    pub icon: &'static str,
}
pub const SERVICES: [Service; 4] = [
    Service {
        name: "Chat",
        url: "https://chat.google.com/",
        icon: include_str!("../assets/chat.svg"),
    },
    Service {
        name: "Mail",
        url: "https://mail.google.com/mail/u/0/",
        icon: include_str!("../assets/gmail.svg"),
    },
    Service {
        name: "Drive",
        url: "https://drive.google.com/drive/u/0/",
        icon: include_str!("../assets/drive.svg"),
    },
    Service {
        name: "Calendar",
        url: "https://calendar.google.com/calendar/u/0/r",
        icon: include_str!("../assets/calendar.svg"),
    },
];

fn parse(raw: &str) -> Option<Url> {
    if raw.trim() != raw || raw.contains('\\') || raw.chars().any(char::is_control) {
        return None;
    }
    let bytes = raw.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return None;
        }
    }
    if percent_decode_str(raw)
        .decode_utf8_lossy()
        .chars()
        .any(char::is_control)
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.scheme() == "https" {
        // Check the original authority before WHATWG parsing can repair or normalize it.
        let (_, rest) = raw.split_once(':')?;
        let rest = rest.strip_prefix("//")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        if authority.is_empty()
            || authority.contains(['@', '%'])
            || authority.chars().any(char::is_whitespace)
            || url.host_str().is_none()
        {
            return None;
        }
    }
    Some(url)
}
fn secure_host(url: &Url) -> Option<&str> {
    (url.scheme() == "https" && url.port_or_known_default() == Some(443))
        .then(|| url.host_str())
        .flatten()
}
pub fn trusted_service(raw: &str) -> bool {
    parse(raw)
        .and_then(|u| {
            secure_host(&u).map(|h| {
                matches!(
                    h,
                    "chat.google.com"
                        | "mail.google.com"
                        | "drive.google.com"
                        | "calendar.google.com"
                )
            })
        })
        .unwrap_or(false)
}
pub fn account_url(raw: &str) -> bool {
    parse(raw).is_some_and(|u| secure_host(&u) == Some("accounts.google.com"))
}
pub fn service_for_url(raw: &str) -> Option<usize> {
    let u = parse(raw)?;
    let path = u.path();
    match secure_host(&u)? {
        "chat.google.com" => {
            let mut parts = path.strip_prefix('/')?.split('/');
            let first = parts.next()?;
            if first == "app"
                || (first == "u"
                    && parts
                        .next()
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())))
            {
                Some(0)
            } else {
                None
            }
        }
        "mail.google.com" if path.starts_with("/mail/") => Some(1),
        "drive.google.com" if path.starts_with("/drive/") => Some(2),
        "calendar.google.com" if path.starts_with("/calendar/") => Some(3),
        _ => None,
    }
}
pub fn notification_service(raw: &str) -> Option<usize> {
    let u = parse(raw)?;
    match secure_host(&u)? {
        "chat.google.com" => Some(0),
        "mail.google.com" => Some(1),
        _ => None,
    }
}
pub fn route(raw: &str, authentication: bool, initial_popup: bool) -> Route {
    let Some(u) = parse(raw) else {
        return Route::Block;
    };
    if initial_popup && raw == "about:blank" {
        return Route::Internal;
    }
    if u.scheme() == "mailto" {
        return if u.host_str().is_none() && !u.path().is_empty() && !u.path().starts_with('/') {
            Route::External
        } else {
            Route::Block
        };
    }
    if u.scheme() != "https" || u.host_str().is_none() {
        return Route::Block;
    }
    if matches!(
        u.host_str(),
        Some("docs.google.com" | "sheets.google.com" | "slides.google.com")
    ) {
        return Route::External;
    }
    if authentication || trusted_service(raw) || account_url(raw) {
        return Route::Internal;
    }
    let host = secure_host(&u);
    if matches!(host, Some("myaccount.google.com" | "contacts.google.com")) {
        return Route::Internal;
    }
    let mut parts: Vec<_> = u.path().split('/').skip(1).collect();
    if parts.first() == Some(&"intl") && parts.get(1).is_some_and(|s| !s.is_empty()) {
        parts.drain(..2);
    }
    if host == Some("workspace.google.com") && parts.first() == Some(&"products") {
        parts.remove(0);
    }
    let product = parts.first().copied().unwrap_or("");
    if (host == Some("workspace.google.com")
        && matches!(product, "gmail" | "drive" | "chat" | "calendar"))
        || (host == Some("www.google.com")
            && (matches!(product, "drive" | "chat" | "calendar")
                || (product == "gmail" && parts.get(1) == Some(&"about"))))
    {
        return Route::Internal;
    }
    Route::External
}
pub fn allows_download(destination: &str, source: &str) -> bool {
    if !trusted_service(source) {
        return false;
    }
    let Some(dest) = parse(destination) else {
        return false;
    };
    if dest.scheme() == "https" {
        return true;
    }
    let Some(origin) = destination.strip_prefix("blob:").and_then(parse) else {
        return false;
    };
    let Some(source) = parse(source) else {
        return false;
    };
    let path = percent_decode_str(origin.path()).decode_utf8_lossy();
    secure_host(&origin).is_some()
        && secure_host(&origin) == secure_host(&source)
        && origin.query().is_none()
        && origin.fragment().is_none()
        && path.starts_with('/')
        && path.len() > 1
        && !path[1..].contains('/')
}
pub fn attachment_candidate(destination: &str, source: &str) -> bool {
    if !allows_download(destination, source) {
        return false;
    }
    if destination.starts_with("blob:") {
        return true;
    }
    parse(destination)
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "drive.usercontent.google.com"
                || h == "googleusercontent.com"
                || h.ends_with(".googleusercontent.com")
        })
}
pub fn safe_filename(suggested: &str) -> String {
    let normalized = suggested.replace('\\', "/");
    let cleaned: String = normalized
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = cleaned.trim();
    if matches!(name, "" | "." | "..") {
        "download".into()
    } else {
        name.into()
    }
}
