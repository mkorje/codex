use std::fmt::Write;
use std::iter;
use std::iter::Peekable;
use std::path::Path;

use self::shared::ModifierSet;

type StrResult<T> = Result<T, String>;

#[path = "src/shared.rs"]
mod shared;

/// A module of definitions.
struct Module<'a>(Vec<(&'a str, Binding<'a>)>);

impl<'a> Module<'a> {
    fn new(mut list: Vec<(&'a str, Binding<'a>)>) -> Self {
        list.sort_by_key(|&(name, _)| name);
        Self(list)
    }
}

/// A definition bound in a module, with metadata.
struct Binding<'a> {
    def: Def<'a>,
    deprecation: Option<&'a str>,
}

/// A definition in a module.
enum Def<'a> {
    Symbol(Symbol<'a>),
    Module(Module<'a>),
}

/// A symbol, either a leaf or with modifiers with optional deprecation.
enum Symbol<'a> {
    Single(char),
    Multi(Vec<(ModifierSet<&'a str>, char, Option<&'a str>)>),
}

/// A single line during parsing.
#[derive(Debug, Copy, Clone)]
enum Line<'a> {
    Blank,
    Deprecated(&'a str),
    ModuleStart(&'a str),
    ModuleEnd,
    Symbol(&'a str, Option<char>),
    Variant(ModifierSet<&'a str>, char),
    Eof,
}

#[derive(Debug, Copy, Clone)]
enum Declaration<'a> {
    ModuleStart(&'a str, Option<&'a str>),
    ModuleEnd,
    Symbol(&'a str, Option<char>, Option<&'a str>),
    Variant(ModifierSet<&'a str>, char, Option<&'a str>),
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let mut buf = String::new();
    process(&mut buf, Path::new("src/modules/sym.txt"), "SYM", "Named general symbols.");
    process(&mut buf, Path::new("src/modules/emoji.txt"), "EMOJI", "Named emoji.");

    let out = std::env::var_os("OUT_DIR").unwrap();
    let dest = Path::new(&out).join("out.rs");
    std::fs::write(&dest, buf).unwrap();
}

/// Processes a single file and turns it into a global module.
fn process(buf: &mut String, file: &Path, name: &str, desc: &str) {
    println!("cargo::rerun-if-changed={}", file.display());

    let text = std::fs::read_to_string(file).unwrap();
    let mut line_nr = 0;
    let mut deprecation = None;
    let mut iter = text
        .lines()
        .inspect(|_| line_nr += 1)
        .map(tokenize)
        .chain(iter::once(Ok(Line::Eof)))
        .filter_map(|line| match line {
            Err(message) => Some(Err(message)),
            Ok(Line::Blank) => None,
            Ok(Line::Deprecated(message)) => {
                if deprecation.is_some() {
                    Some(Err(String::from("duplicate `@deprecated:`")))
                } else {
                    deprecation = Some(message);
                    None
                }
            }
            Ok(Line::ModuleStart(name)) => {
                Some(Ok(Declaration::ModuleStart(name, deprecation.take())))
            }
            Ok(Line::ModuleEnd) => {
                if deprecation.is_some() {
                    Some(Err(String::from("dangling `@deprecated:`")))
                } else {
                    Some(Ok(Declaration::ModuleEnd))
                }
            }
            Ok(Line::Symbol(name, c)) => {
                Some(Ok(Declaration::Symbol(name, c, deprecation.take())))
            }
            Ok(Line::Variant(modifiers, c)) => {
                Some(Ok(Declaration::Variant(modifiers, c, deprecation.take())))
            }
            Ok(Line::Eof) => {
                deprecation.map(|_| Err(String::from("dangling `@deprecated:`")))
            }
        })
        .peekable();

    let module = match parse(&mut iter) {
        Ok(defs) => Module::new(defs),
        Err(e) => {
            let message = format!("{}:{}: {e}", file.display(), line_nr);
            println!("cargo::warning={message}");
            std::process::exit(1);
        }
    };

    write!(buf, "#[doc = {desc:?}] pub const {name}: Module = ").unwrap();
    encode(buf, &module);
    buf.push(';');
}

/// Tokenizes and classifies a line.
fn tokenize(line: &str) -> StrResult<Line> {
    // Strip comments.
    let line = line.split_once("//").map_or(line, |(head, _)| head);

    // Ignore empty lines.
    let line = line.trim();
    if line.is_empty() {
        return Ok(Line::Blank);
    }

    let (head, tail) = match line.split_once(' ') {
        Some((a, b)) => (a, Some(b)),
        None => (line, None),
    };

    Ok(if head == "@deprecated:" {
        Line::Deprecated(tail.ok_or("missing deprecation message")?.trim())
    } else if tail == Some("{") {
        validate_ident(head)?;
        Line::ModuleStart(head)
    } else if head == "}" && tail.is_none() {
        Line::ModuleEnd
    } else if let Some(rest) = head.strip_prefix('.') {
        for part in rest.split('.') {
            validate_ident(part)?;
        }
        let c = decode_char(tail.ok_or("missing char")?)?;
        Line::Variant(ModifierSet::from_raw_dotted(rest), c)
    } else {
        validate_ident(head)?;
        let c = tail.map(decode_char).transpose()?;
        Line::Symbol(head, c)
    })
}

/// Ensures that a string is a valid identifier. In `codex`, we use very strict
/// rules and allow only alphabetic ASCII chars.
fn validate_ident(string: &str) -> StrResult<()> {
    if !string.is_empty() && string.chars().all(|c| c.is_ascii_alphabetic()) {
        return Ok(());
    }
    Err(format!("invalid identifier: {string:?}"))
}

/// Extracts either a single char or parses a U+XXXX escape.
fn decode_char(text: &str) -> StrResult<char> {
    if let Some(hex) = text.strip_prefix("U+") {
        u32::from_str_radix(hex, 16)
            .ok()
            .and_then(|n| char::try_from(n).ok())
            .ok_or_else(|| format!("invalid unicode escape {text:?}"))
    } else {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(format!("expected exactly one char, found {text:?}")),
        }
    }
}

/// Turns a stream of lines into a list of definitions.
fn parse<'a>(
    p: &mut Peekable<impl Iterator<Item = StrResult<Declaration<'a>>>>,
) -> StrResult<Vec<(&'a str, Binding<'a>)>> {
    let mut defs = vec![];
    loop {
        match p.next().transpose()? {
            None | Some(Declaration::ModuleEnd) => {
                break;
            }
            Some(Declaration::Symbol(name, c, deprecation)) => {
                let mut variants = vec![];
                while let Some(Declaration::Variant(name, c, deprecation)) =
                    p.peek().cloned().transpose()?
                {
                    variants.push((name, c, deprecation));
                    p.next();
                }

                let symbol = if !variants.is_empty() {
                    if let Some(c) = c {
                        variants.insert(0, (ModifierSet::default(), c, None));
                    }
                    Symbol::Multi(variants)
                } else {
                    let c = c.ok_or("symbol needs char or variants")?;
                    Symbol::Single(c)
                };

                defs.push((name, Binding { def: Def::Symbol(symbol), deprecation }));
            }
            Some(Declaration::ModuleStart(name, deprecation)) => {
                let module_defs = parse(p)?;
                defs.push((
                    name,
                    Binding {
                        def: Def::Module(Module::new(module_defs)),
                        deprecation,
                    },
                ));
            }
            other => return Err(format!("expected definition, found {other:?}")),
        }
    }
    Ok(defs)
}

/// Encodes a `Module` into Rust code.
fn encode(buf: &mut String, module: &Module) {
    buf.push_str("Module(&[");
    for (name, entry) in &module.0 {
        write!(buf, "({name:?}, Binding {{ def: ").unwrap();
        match &entry.def {
            Def::Module(module) => {
                buf.push_str("Def::Module(");
                encode(buf, module);
                buf.push(')');
            }
            Def::Symbol(symbol) => {
                buf.push_str("Def::Symbol(Symbol::");
                match symbol {
                    Symbol::Single(c) => write!(buf, "Single({c:?})").unwrap(),
                    Symbol::Multi(list) => write!(buf, "Multi(&{list:?})").unwrap(),
                }
                buf.push(')');
            }
        }
        write!(buf, ", deprecation: {:?} }}),", entry.deprecation).unwrap();
    }
    buf.push_str("])");
}
