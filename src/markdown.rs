//! Markdown, as agents write it, in the HTML Telegram takes:
//! `https://core.telegram.org/bots/api#html-style`.
//! Telegram has no headings, lists or tables, so they become bold lines, bullets and plain text.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Escapes text for Telegram's HTML.
pub fn escape(text: &str) -> String {
  text
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}

/// `markdown` as Telegram's HTML.
pub fn to_html(markdown: &str) -> String {
  let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
  let mut out = String::new();
  // Each open list: its next number, where it is ordered.
  let mut lists: Vec<Option<u64>> = Vec::new();
  let mut in_cell = false;
  for event in Parser::new_ext(markdown, options) {
    match event {
      Event::Start(tag) => match tag {
        Tag::Paragraph => {}
        Tag::Heading { .. } => out.push_str("<b>"),
        Tag::BlockQuote(_) => out.push_str("<blockquote>"),
        Tag::CodeBlock(kind) => match kind {
          CodeBlockKind::Fenced(language) if !language.trim().is_empty() => {
            let language = language.split_whitespace().next().unwrap_or_default();
            out.push_str(&format!(
              "<pre><code class=\"language-{}\">",
              escape(language).replace('"', "")
            ));
          }
          _ => out.push_str("<pre><code>"),
        },
        Tag::List(start) => {
          if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
          }
          lists.push(start);
        }
        Tag::Item => {
          let depth = lists.len().saturating_sub(1);
          out.push_str(&"  ".repeat(depth));
          match lists.last_mut() {
            Some(Some(n)) => {
              out.push_str(&format!("{n}. "));
              *n += 1;
            }
            _ => out.push_str("• "),
          }
        }
        Tag::Emphasis => out.push_str("<i>"),
        Tag::Strong => out.push_str("<b>"),
        Tag::Strikethrough => out.push_str("<s>"),
        Tag::Link { dest_url, .. } => out.push_str(&format!(
          "<a href=\"{}\">",
          escape(&dest_url).replace('"', "&quot;")
        )),
        Tag::Image { dest_url, .. } => out.push_str(&format!(
          "<a href=\"{}\">",
          escape(&dest_url).replace('"', "&quot;")
        )),
        Tag::TableCell => {
          if in_cell {
            out.push_str(" | ");
          }
          in_cell = true;
        }
        _ => {}
      },
      Event::End(tag) => match tag {
        TagEnd::Paragraph => out.push_str("\n\n"),
        TagEnd::Heading(level) => {
          out.push_str("</b>");
          out.push_str(if level == HeadingLevel::H1 {
            "\n\n"
          } else {
            "\n"
          });
        }
        TagEnd::BlockQuote(_) => {
          trim_end(&mut out);
          out.push_str("</blockquote>\n\n");
        }
        TagEnd::CodeBlock => {
          trim_end(&mut out);
          out.push_str("</code></pre>\n\n");
        }
        TagEnd::List(_) => {
          lists.pop();
          if lists.is_empty() {
            trim_end(&mut out);
            out.push_str("\n\n");
          }
        }
        TagEnd::Item => {
          trim_end(&mut out);
          out.push('\n');
        }
        TagEnd::Emphasis => out.push_str("</i>"),
        TagEnd::Strong => out.push_str("</b>"),
        TagEnd::Strikethrough => out.push_str("</s>"),
        TagEnd::Link | TagEnd::Image => out.push_str("</a>"),
        TagEnd::TableRow | TagEnd::TableHead => {
          in_cell = false;
          out.push('\n');
        }
        TagEnd::Table => out.push('\n'),
        _ => {}
      },
      Event::Text(text) => out.push_str(&escape(&text)),
      Event::Code(code) => out.push_str(&format!("<code>{}</code>", escape(&code))),
      Event::Html(html) | Event::InlineHtml(html) => out.push_str(&escape(&html)),
      Event::SoftBreak | Event::HardBreak => out.push('\n'),
      Event::Rule => out.push_str("———\n\n"),
      Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
      Event::FootnoteReference(name) => out.push_str(&format!("[{}]", escape(&name))),
      Event::InlineMath(m) | Event::DisplayMath(m) => out.push_str(&escape(&m)),
    }
  }
  trim_end(&mut out);
  out
}

fn trim_end(out: &mut String) {
  while out.ends_with(['\n', ' ']) {
    out.pop();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn converts_the_usual() {
    assert_eq!(
      to_html("**Done.** The `x < y` test"),
      "<b>Done.</b> The <code>x &lt; y</code> test"
    );
    assert_eq!(to_html("# Title\n\nText"), "<b>Title</b>\n\nText");
    assert_eq!(to_html("- a\n- b\n\nafter"), "• a\n• b\n\nafter");
    assert_eq!(to_html("1. a\n2. b"), "1. a\n2. b");
    assert_eq!(
      to_html("```rust\nfn f() {}\n```"),
      "<pre><code class=\"language-rust\">fn f() {}</code></pre>"
    );
    assert_eq!(
      to_html("[x](https://a.b/?q=1&r=2)"),
      "<a href=\"https://a.b/?q=1&amp;r=2\">x</a>"
    );
    assert_eq!(to_html("<script>"), "&lt;script&gt;");
  }
}
