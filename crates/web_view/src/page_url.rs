//! Turns what the user typed in "web: open url" into the URL a page tab opens (issue #31).
//!
//! Text with a scheme (`://`) is parsed as it is. Text without one gets `https://` in front, and is accepted only when its host looks like a host: it has a dot, is `localhost`, or is an IP address.
//! Text with spaces is never a URL. On an error the prompt shows the message and nothing opens.

use url::{Host, Url};

pub fn normalize_page_url(input: &str) -> Result<String, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("Type a URL to open.".into());
    }
    if text.chars().any(char::is_whitespace) {
        return Err(format!("Not a URL: {text:?} contains spaces."));
    }
    if text.contains("://") {
        let url = Url::parse(text).map_err(|error| format!("Not a URL: {error}."))?;
        if matches!(url.scheme(), "http" | "https") && url.host().is_none() {
            return Err(format!("Not a URL: {text:?} has no host."));
        }
        return Ok(url.to_string());
    }
    // `Url::parse("localhost:8000")` would read "localhost" as a scheme, so a text without "://" is always given one first.
    let url = Url::parse(&format!("https://{text}"))
        .map_err(|_| format!("Not a URL: {text:?}."))?;
    let looks_like_a_host = match url.host() {
        Some(Host::Domain(domain)) => {
            domain == "localhost"
                || (domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.'))
        }
        Some(Host::Ipv4(_)) | Some(Host::Ipv6(_)) => true,
        None => false,
    };
    // A user name in the "host" means the text had a scheme-like prefix such as "mailto:".
    if !looks_like_a_host || !url.username().is_empty() || url.password().is_some() {
        return Err(format!("Not a URL: {text:?}."));
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_with_a_scheme_is_kept() {
        assert_eq!(
            normalize_page_url("http://127.0.0.1:8000/tracker/PAY-1234").unwrap(),
            "http://127.0.0.1:8000/tracker/PAY-1234"
        );
        assert_eq!(
            normalize_page_url("  https://wiki.example.org/Home \n").unwrap(),
            "https://wiki.example.org/Home"
        );
    }

    #[test]
    fn a_url_without_a_scheme_gets_https() {
        assert_eq!(
            normalize_page_url("tracker.example.com/PAY-1234").unwrap(),
            "https://tracker.example.com/PAY-1234"
        );
    }

    #[test]
    fn localhost_and_ip_addresses_with_ports_get_https() {
        assert_eq!(
            normalize_page_url("localhost:8000/x").unwrap(),
            "https://localhost:8000/x"
        );
        assert_eq!(
            normalize_page_url("127.0.0.1:8000/tracker").unwrap(),
            "https://127.0.0.1:8000/tracker"
        );
        assert_eq!(normalize_page_url("[::1]:8000").unwrap(), "https://[::1]:8000/");
    }

    #[test]
    fn text_with_spaces_is_an_error() {
        assert!(normalize_page_url("fix invoice rounding").is_err());
        assert!(normalize_page_url("http://example.com/a b").is_err());
    }

    #[test]
    fn empty_text_is_an_error() {
        assert!(normalize_page_url("").is_err());
        assert!(normalize_page_url("   ").is_err());
    }

    #[test]
    fn a_single_word_is_not_a_host() {
        assert!(normalize_page_url("invoice").is_err());
        assert!(normalize_page_url("invoice.").is_err());
    }

    #[test]
    fn scheme_like_text_without_slashes_is_an_error() {
        assert!(normalize_page_url("mailto:someone@example.com").is_err());
        assert!(normalize_page_url("about:blank").is_err());
    }

    #[test]
    fn an_http_url_needs_a_host() {
        assert!(normalize_page_url("http://").is_err());
    }
}
