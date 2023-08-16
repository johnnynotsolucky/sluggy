use crate::lazyfn::LazyFn;
use comrak::adapters::SyntaxHighlighterAdapter;
use std::{io, io::Write};
use syntect::{
	html::{ClassStyle, ClassedHTMLGenerator},
	parsing::SyntaxSet,
	util::LinesWithEndings,
};
use tracing::instrument;

static SYNTAX_SET: LazyFn<SyntaxSet> = LazyFn::new(SyntaxSet::load_defaults_newlines);
// LazyFn::new(|| from_binary(include_bytes!("./newlines.packdump")));

// const THEME_SET: LazyFn<ThemeSet> =
//     LazyFn::new(|| from_binary(include_bytes!("./all.themedump")));
pub struct SyntectAdapter;

fn map_lang(lang: Option<&str>) -> &str {
	// TODO use enum or something so we can get default langs and shit. Check how zola does it
	lang.map(|lang| {
		let lang = lang.trim();

		if lang.is_empty() {
			"txt"
		} else {
			match lang {
				"plaintext" | "text" => "txt",
				_ => lang,
			}
		}
	})
	.unwrap_or("txt")
}

impl SyntaxHighlighterAdapter for SyntectAdapter {
	#[inline]
	#[instrument(level = "trace", skip(self, output, code))]
	fn write_highlighted(
		&self,
		output: &mut dyn Write,
		lang: Option<&str>,
		code: &str,
	) -> io::Result<()> {
		// TODO relevant for figuring out how to generate themes and syntaxes and shit
		// let theme_set = &*THEME_SET;
		// for (a, t) in &theme_set.themes {
		// 	println!("{} {:?}", a, t.name);
		// 	let css_dark_file = std::fs::File::create(Path::new(&format!("theme-{a}.css"))).unwrap();
		// 	let mut css_dark_writer = BufWriter::new(&css_dark_file);

		// 	let css_dark = css_for_theme_with_class_style(t, ClassStyle::SpacedPrefixed { prefix: "" }).unwrap();
		// 	writeln!(css_dark_writer, "{}", css_dark).unwrap();
		// }
		// panic!();
		// // create dark color scheme css

		// let set = &*SYNTAX_SET;
		// for s in set.syntaxes() {
		// 	println!("s {} {:?}", s.name, s.file_extensions);

		// }
		// panic!("oops");

		if let Some(syntax) = SYNTAX_SET.find_syntax_by_token(map_lang(lang)) {
			let mut html_generator =
				ClassedHTMLGenerator::new_with_class_style(syntax, &SYNTAX_SET, ClassStyle::Spaced);
			for line in LinesWithEndings::from(code) {
				html_generator
					.parse_html_for_line_which_includes_newline(line)
					.unwrap();
			}

			output.write_all(html_generator.finalize().as_bytes())
		} else {
			output.write_all(code.as_bytes())
		}
	}

	#[inline]
	#[instrument(level = "trace", skip(self, output))]
	fn write_pre_tag(
		&self,
		output: &mut dyn Write,
		attributes: std::collections::HashMap<String, String>,
	) -> io::Result<()> {
		if attributes.contains_key("lang") {
			write!(output, "<pre lang=\"{}\">", attributes["lang"])
		} else {
			output.write_all(b"<pre>")
		}
	}

	#[inline]
	#[instrument(level = "trace", skip(self, output))]
	fn write_code_tag(
		&self,
		output: &mut dyn Write,
		attributes: std::collections::HashMap<String, String>,
	) -> io::Result<()> {
		if attributes.contains_key("class") {
			write!(
				output,
				"<code class=\"highlight code {}\">",
				attributes["class"]
			)
		} else {
			output.write_all(b"<code class=\"highlight code\">")
		}
	}
}
