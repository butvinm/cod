//! The small Cod action bridge for agent-made pages (#43), built on #31/#32 pages and #37/#38
//! `codctl`.
//!
//! An agent page is built once, in Rust, from the HTML `codctl open --html` was given: the page's
//! own inline scripts are stripped unless `--js` was given, and Cod's own bootstrap script is
//! always appended, regardless. The bootstrap defines `window.cod.call(name, args)` and wires
//! every `[data-cod]` element to it; a call becomes one `console.log` line behind a sentinel
//! prefix, which Cod's side (`offscreen.rs`) reads apart from the page's own console output and
//! checks against an allowlist here, in Cod, never in the page. The result is served as a `data:`
//! URL, so the page never gets file-system access of its own and a navigation away from it loses
//! the bridge (it only ever existed in that one document).

use gpui::{App, Global};
use serde_json::Value;

use crate::pages::PageId;

/// Every line the page's bootstrap script prints on Cod's behalf starts with this, so Cod can
/// tell a bridge call from the page's own console output. No real page is expected to start a log
/// line this way.
pub const BRIDGE_PREFIX: &str = "cod:bridge:";

/// Every action an agent page may ask for; anything else is refused and logged (#43 AC3, AC4).
pub const ALLOWED_ACTIONS: &[&str] = &["submit", "open-file", "focus-session"];

/// One call from a page's bootstrap script.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeCall {
    pub name: String,
    pub args: Value,
}

/// Reads a bridge call from one console line, if it is one.
pub fn parse_bridge_message(line: &str) -> Option<BridgeCall> {
    let payload = line.strip_prefix(BRIDGE_PREFIX)?;
    let value: Value = serde_json::from_str(payload).ok()?;
    let name = value.get("name")?.as_str()?.to_owned();
    let args = value.get("args").cloned().unwrap_or(Value::Null);
    Some(BridgeCall { name, args })
}

/// A string argument of a bridge call, or empty when it is missing.
pub fn arg_str(call: &BridgeCall, key: &str) -> String {
    call.args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Whether `name` is in the allowlist Cod checks before acting on a bridge call (#43 AC3, AC4,
/// security requirement #7: the allowlist is checked here, not in the page).
pub fn is_allowed(name: &str) -> bool {
    ALLOWED_ACTIONS.contains(&name)
}

/// Splits `path:line:column` or `path:line` the way `codctl open` does, so a page's
/// `data-cod-path` can point at a line like `src/invoice.rs:18`.
pub fn split_path_position(target: &str) -> (&str, Option<u32>, Option<u32>) {
    let number = |text: &str| {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u32>().ok().filter(|number| *number > 0)
    };
    if let Some((rest, last)) = target.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, middle)) = rest.rsplit_once(':')
            && let Some(middle) = number(middle)
            && !path.is_empty()
        {
            return (path, Some(middle), Some(last));
        }
        if !rest.is_empty() {
            return (rest, Some(last), None);
        }
    }
    (target, None, None)
}

/// The current theme's colors and font, injected as CSS custom properties (#43 AC6).
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeVars {
    pub background: String,
    pub text: String,
    pub accent: String,
    pub font: String,
}

impl ThemeVars {
    /// The `document.documentElement.style.setProperty` calls that apply these variables; used
    /// both in the page's bootstrap script and to update a live page after a theme switch,
    /// without a reload.
    fn set_script(&self) -> String {
        format!(
            "document.documentElement.style.setProperty('--cod-background',{background});\
             document.documentElement.style.setProperty('--cod-text',{text});\
             document.documentElement.style.setProperty('--cod-accent',{accent});\
             document.documentElement.style.setProperty('--cod-font',{font});",
            background = js_string(&self.background),
            text = js_string(&self.text),
            accent = js_string(&self.accent),
            font = js_string(&self.font),
        )
    }
}

/// Reads the colors and font the running Cod shows now, as `ThemeVars`.
pub fn current_theme_vars(cx: &App) -> ThemeVars {
    use settings::Settings as _;
    use theme::ActiveTheme as _;

    let colors = cx.theme().colors();
    let css = |color: gpui::Hsla| {
        let rgba = color.to_rgb();
        format!(
            "rgba({}, {}, {}, {:.3})",
            (rgba.r * 255.0).round() as u8,
            (rgba.g * 255.0).round() as u8,
            (rgba.b * 255.0).round() as u8,
            rgba.a,
        )
    };
    let font = theme_settings::ThemeSettings::get_global(cx)
        .buffer_font
        .family
        .to_string();
    ThemeVars {
        background: css(colors.background),
        text: css(colors.text),
        accent: css(colors.text_accent),
        font,
    }
}

/// A JavaScript single-quoted string literal of `text`, escaping backslashes, quotes and newlines.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

/// Cod's bootstrap script for an agent page (#43 AC3, AC4, AC6): theme variables, `window.cod.call`
/// and the `[data-cod]` click wiring. It always runs, even when the page's own inline scripts do
/// not: it is Cod's own code, appended after they are stripped, never the page's.
pub fn bootstrap_script(vars: &ThemeVars) -> String {
    format!(
        "<script>(function(){{\
         {theme_vars}\
         function codSend(name,args){{console.log({prefix}+JSON.stringify({{name:name,args:args||{{}}}}));}}\
         window.cod={{call:codSend}};\
         document.addEventListener('click',function(e){{\
         var el=e.target&&e.target.closest?e.target.closest('[data-cod]'):null;\
         if(!el)return;\
         var name=el.getAttribute('data-cod');\
         var args={{}};\
         if(name==='submit'){{\
         var sel=el.getAttribute('data-cod-value-from');\
         var field=sel?document.querySelector(sel):null;\
         args.value=field?field.value:'';\
         }}else if(name==='open-file'){{\
         args.path=el.getAttribute('data-cod-path')||'';\
         }}else if(name==='focus-session'){{\
         args.session=el.getAttribute('data-cod-session')||el.textContent||'';\
         }}\
         e.preventDefault();\
         codSend(name,args);\
         }},true);\
         }})();</script>",
        theme_vars = vars.set_script(),
        prefix = js_string(BRIDGE_PREFIX),
    )
}

/// The script that updates an already-open page's theme variables without a reload (#43 AC6).
pub fn theme_update_script(vars: &ThemeVars) -> String {
    vars.set_script()
}

/// Removes every `<script>...</script>` block from `html`, case-insensitively, leaving everything
/// else as it was (#43 AC4: inline scripts do not run unless `--js` was given).
pub fn strip_inline_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut lower_rest = lower.as_str();
    loop {
        let Some(start) = lower_rest.find("<script") else {
            out.push_str(rest);
            break;
        };
        // A tag name can continue past "<script" (for example "<scripture>"); only a real
        // script element starts here, where the name ends at whitespace, '>' or '/'.
        let after_name = lower_rest[start + "<script".len()..].chars().next();
        if !after_name.is_some_and(|ch| ch.is_whitespace() || ch == '>' || ch == '/') {
            let keep_to = start + "<script".len();
            out.push_str(&rest[..keep_to]);
            rest = &rest[keep_to..];
            lower_rest = &lower_rest[keep_to..];
            continue;
        }
        out.push_str(&rest[..start]);
        let after_start = match lower_rest[start..].find('>') {
            Some(offset) => start + offset + 1,
            None => {
                // An unterminated opening tag: there is no script content left to keep.
                break;
            }
        };
        let end = lower_rest[after_start..]
            .find("</script>")
            .map(|offset| after_start + offset + "</script>".len())
            .unwrap_or(lower_rest.len());
        rest = &rest[end..];
        lower_rest = &lower_rest[end..];
    }
    out
}

/// The byte offset just past the first opening tag `<name ...>` (ASCII case-insensitive) in `lower`, the lowercased document, or None without one. A longer tag name that only starts with `name` (`<header>` for `head`) does not count.
fn after_open_tag(lower: &str, name: &str) -> Option<usize> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(found) = lower[from..].find(&open) {
        let start = from + found;
        let after_name = start + open.len();
        let next = lower[after_name..].chars().next();
        if next.is_some_and(|ch| ch.is_ascii_whitespace() || ch == '>' || ch == '/') {
            return lower[after_name..].find('>').map(|offset| after_name + offset + 1);
        }
        from = after_name;
    }
    None
}

/// Inserts `script` as early as the document allows: right after the opening `<head>` tag, else after `<html>`, else after the `<!doctype>`, else at the very start. Cod's bootstrap must run before any of the page's own scripts, so a `--js` page script can call `window.cod` while the document loads (#43 AC4).
pub fn inject_at_document_start(html: &str, script: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let at = after_open_tag(&lower, "head")
        .or_else(|| after_open_tag(&lower, "html"))
        .or_else(|| {
            lower
                .trim_start()
                .starts_with("<!doctype")
                .then(|| lower.find('>').map(|offset| offset + 1))
                .flatten()
        })
        .unwrap_or(0);
    format!("{}{}{}", &html[..at], script, &html[at..])
}

/// Cod's whole processing of an agent page's source (#43 AC1, AC4, AC6): strips the page's own inline scripts unless `allow_js`, then inserts Cod's bootstrap script before everything else in the document.
pub fn build_agent_document(source_html: &str, allow_js: bool, vars: &ThemeVars) -> String {
    let body = if allow_js {
        source_html.to_owned()
    } else {
        strip_inline_scripts(source_html)
    };
    inject_at_document_start(&body, &bootstrap_script(vars))
}

/// A `data:` URL that loads `html` as a document (#43 AC1, AC5, open question 1): used instead of
/// `file://` so an agent page never gets direct file-system access of its own, and the bridge
/// cannot be carried to a page opened any other way.
pub fn data_url(html: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
    format!("data:text/html;charset=utf-8;base64,{encoded}")
}

/// What `codctl open --html --wait` gets when the page tab ends (#43 AC2).
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    /// The user submitted this value; the page tab is already closed.
    Submitted(String),
    /// The user closed the tab instead of submitting.
    Closed,
}

#[derive(Default)]
struct Waiters(std::collections::BTreeMap<PageId, futures::channel::oneshot::Sender<SubmitOutcome>>);

impl Global for Waiters {}

/// Registers a waiter for the page's submit or close, for `codctl open --html --wait` (#43 AC2).
pub fn wait_for_submit(page: PageId, cx: &mut App) -> futures::channel::oneshot::Receiver<SubmitOutcome> {
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.default_global::<Waiters>().0.insert(page, tx);
    rx
}

fn resolve_waiter(page: &PageId, outcome: SubmitOutcome, cx: &mut App) {
    if let Some(sender) = cx.default_global::<Waiters>().0.remove(page) {
        sender.send(outcome).ok();
    }
}

/// Tells a pending waiter the user submitted `value`; a no-op without one.
pub fn submit(page: &PageId, value: String, cx: &mut App) {
    resolve_waiter(page, SubmitOutcome::Submitted(value), cx);
}

/// Tells a pending waiter the tab closed instead; a no-op without one (not waited for, or already
/// resolved by a submit).
pub fn closed(page: &PageId, cx: &mut App) {
    resolve_waiter(page, SubmitOutcome::Closed, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> ThemeVars {
        ThemeVars {
            background: "rgba(10, 10, 10, 1.000)".into(),
            text: "rgba(255, 255, 255, 1.000)".into(),
            accent: "rgba(80, 120, 250, 1.000)".into(),
            font: "Zed Sans".into(),
        }
    }

    #[test]
    fn bridge_messages_are_told_apart_from_normal_console_output() {
        assert!(parse_bridge_message("hello").is_none());
        assert!(parse_bridge_message("cod:bridge:not json").is_none());
        let call =
            parse_bridge_message(r#"cod:bridge:{"name":"submit","args":{"value":"half-even"}}"#)
                .unwrap();
        assert_eq!(call.name, "submit");
        assert_eq!(arg_str(&call, "value"), "half-even");
    }

    #[test]
    fn only_the_allowed_actions_pass() {
        assert!(is_allowed("submit"));
        assert!(is_allowed("open-file"));
        assert!(is_allowed("focus-session"));
        assert!(!is_allowed("run-shell"));
        assert!(!is_allowed(""));
    }

    #[test]
    fn file_positions_split_like_codctl_open() {
        assert_eq!(split_path_position("src/invoice.rs:18:9"), ("src/invoice.rs", Some(18), Some(9)));
        assert_eq!(split_path_position("src/invoice.rs:18"), ("src/invoice.rs", Some(18), None));
        assert_eq!(split_path_position("src/invoice.rs"), ("src/invoice.rs", None, None));
    }

    #[test]
    fn inline_scripts_are_stripped_but_other_tags_survive() {
        let html = "<body><h1 id=\"h\">Ask</h1><script>document.getElementById('h').textContent='changed';</script><p>end</p></body>";
        let stripped = strip_inline_scripts(html);
        assert!(!stripped.contains("<script"));
        assert!(stripped.contains("<h1 id=\"h\">Ask</h1>"));
        assert!(stripped.contains("<p>end</p>"));
    }

    #[test]
    fn a_tag_that_only_starts_with_script_is_left_alone() {
        let html = "<scripture>not a script</scripture>";
        assert_eq!(strip_inline_scripts(html), html);
    }

    #[test]
    fn the_bootstrap_script_starts_the_head() {
        let html = "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><h1>Ask</h1></body></html>";
        let built = build_agent_document(html, false, &vars());
        assert!(built.contains("<h1>Ask</h1>"));
        assert!(built.starts_with("<!doctype html><html><head><script>"));
        assert!(built.find("window.cod").unwrap() < built.find("<meta").unwrap());
    }

    #[test]
    fn the_bootstrap_script_runs_before_the_pages_own_scripts() {
        // A page script that calls window.cod while the document loads must find it defined (#43 AC4).
        let html = "<!doctype html><html><head><script>var early = 1;</script></head><body><script>window.cod.call('submit', {value: 'x'});</script></body></html>";
        let built = build_agent_document(html, true, &vars());
        let bootstrap = built.find("window.cod={").unwrap();
        assert!(bootstrap < built.find("var early").unwrap());
        assert!(bootstrap < built.find("window.cod.call('submit'").unwrap());
    }

    #[test]
    fn the_bootstrap_script_goes_first_in_a_document_without_head_or_html_tags() {
        assert!(build_agent_document("<p>hi</p>", false, &vars()).starts_with("<script>"));
        let built = build_agent_document("<!DOCTYPE html><p>hi</p>", false, &vars());
        assert!(built.starts_with("<!DOCTYPE html><script>"));
        let built = build_agent_document("<HTML lang=\"en\"><Body><p>hi</p></Body></HTML>", false, &vars());
        assert!(built.starts_with("<HTML lang=\"en\"><script>"));
        let built = build_agent_document("<html><header>x</header></html>", false, &vars());
        assert!(built.starts_with("<html><script>"));
    }

    #[test]
    fn js_is_kept_only_with_allow_js() {
        let html = "<body><script>window.ranInlineScript = true;</script></body>";
        assert!(!build_agent_document(html, false, &vars()).contains("ranInlineScript"));
        assert!(build_agent_document(html, true, &vars()).contains("ranInlineScript"));
    }

    #[test]
    fn data_urls_decode_back_to_the_same_html() {
        use base64::Engine as _;
        let html = "<p>hi</p>";
        let url = data_url(html);
        let encoded = url.strip_prefix("data:text/html;charset=utf-8;base64,").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), html);
    }

    #[test]
    fn theme_variables_are_set_as_javascript_string_literals() {
        let script = theme_update_script(&vars());
        assert!(script.contains("--cod-background"));
        assert!(script.contains("'rgba(10, 10, 10, 1.000)'"));
        assert!(script.contains("Zed Sans"));
    }

    #[test]
    fn javascript_string_literals_escape_quotes_and_backslashes() {
        assert_eq!(js_string("a'b\\c"), "'a\\'b\\\\c'");
    }
}
