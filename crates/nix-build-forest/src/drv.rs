//! Minimal ATerm parser for Nix `.drv` files.
//!
//! Only the stable `Derive(outputs,inputDrvs,inputSrcs,...)` fields needed by
//! the forest are retained. The parser still consumes the complete term so
//! unfamiliar builders, arguments, and environment strings cannot desync it.

use std::collections::BTreeMap;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    pub outputs: BTreeMap<String, String>,
    pub input_drvs: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    App(String, Vec<Term>),
    List(Vec<Term>),
    Tuple(Vec<Term>),
    Str(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ATerm parse error at byte {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for ParseError {}

struct Parser<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            at: 0,
        }
    }

    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.at,
            message,
        }
    }

    fn skip_ws(&mut self) {
        while self.input.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        self.skip_ws();
        if self.input.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        self.take(byte)
            .then_some(())
            .ok_or_else(|| self.error("unexpected token"))
    }

    fn term(&mut self) -> Result<Term, ParseError> {
        self.skip_ws();
        match self.input.get(self.at).copied() {
            Some(b'"') => self.string().map(Term::Str),
            Some(b'[') => self.sequence(b'[', b']').map(Term::List),
            Some(b'(') => self.sequence(b'(', b')').map(Term::Tuple),
            Some(_) => {
                let name = self.ident()?;
                self.expect(b'(')?;
                let args = self.comma_terms(b')')?;
                Ok(Term::App(name, args))
            }
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn sequence(&mut self, open: u8, close: u8) -> Result<Vec<Term>, ParseError> {
        self.expect(open)?;
        self.comma_terms(close)
    }

    fn comma_terms(&mut self, close: u8) -> Result<Vec<Term>, ParseError> {
        let mut terms = Vec::new();
        self.skip_ws();
        if self.take(close) {
            return Ok(terms);
        }
        loop {
            terms.push(self.term()?);
            if self.take(close) {
                return Ok(terms);
            }
            self.expect(b',')?;
        }
    }

    fn ident(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        let start = self.at;
        while self
            .input
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.error("expected constructor name"));
        }
        Ok(String::from_utf8_lossy(&self.input[start..self.at]).into_owned())
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(byte) = self.input.get(self.at).copied() else {
                return Err(self.error("unterminated string"));
            };
            self.at += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(escaped) = self.input.get(self.at).copied() else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.at += 1;
                    match escaped {
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        other => out.push(char::from(other)),
                    }
                }
                other => out.push(char::from(other)),
            }
        }
    }
}

fn as_str(term: &Term) -> Option<&str> {
    let Term::Str(value) = term else { return None };
    Some(value)
}

/// Parse the output paths and `inputDrvs` graph edge set from a `.drv`.
pub fn parse_derivation(input: &str) -> Result<Derivation, ParseError> {
    let mut parser = Parser::new(input);
    let term = parser.term()?;
    parser.skip_ws();
    if parser.at != parser.input.len() {
        return Err(parser.error("trailing input"));
    }
    let Term::App(name, args) = term else {
        return Err(parser.error("expected Derive application"));
    };
    if name != "Derive" || args.len() < 2 {
        return Err(parser.error("expected Derive outputs and inputDrvs"));
    }

    let mut outputs = BTreeMap::new();
    if let Term::List(entries) = &args[0] {
        for entry in entries {
            let Term::Tuple(fields) = entry else { continue };
            if let (Some(name), Some(path)) = (
                fields.first().and_then(as_str),
                fields.get(1).and_then(as_str),
            ) {
                outputs.insert(name.to_string(), path.to_string());
            }
        }
    }

    let mut input_drvs = BTreeMap::new();
    if let Term::List(entries) = &args[1] {
        for entry in entries {
            let Term::Tuple(fields) = entry else { continue };
            let Some(path) = fields.first().and_then(as_str) else {
                continue;
            };
            let names = match fields.get(1) {
                Some(Term::List(names)) => names
                    .iter()
                    .filter_map(as_str)
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            input_drvs.insert(path.to_string(), names);
        }
    }
    Ok(Derivation {
        outputs,
        input_drvs,
    })
}

#[allow(async_fn_in_trait)]
pub trait DrvReader {
    async fn read_drv(&self, path: &str) -> io::Result<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FsDrvReader;

impl DrvReader for FsDrvReader {
    async fn read_drv(&self, path: &str) -> io::Result<String> {
        tokio::fs::read_to_string(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_outputs_and_input_drvs() {
        let drv = parse_derivation(
            r#"Derive([("out","/nix/store/root","","sha256")],[("/nix/store/a.drv",["out"]),("/nix/store/b.drv",["dev","out"])],["/nix/store/source"],"aarch64-darwin","/nix/store/bash",["-e","line\\nnext"],[("name","root")])"#,
        )
        .unwrap();
        assert_eq!(drv.outputs["out"], "/nix/store/root");
        assert_eq!(drv.input_drvs["/nix/store/a.drv"], ["out"]);
        assert_eq!(drv.input_drvs["/nix/store/b.drv"], ["dev", "out"]);
    }

    #[test]
    fn malformed_drv_is_an_error_not_a_panic() {
        assert!(parse_derivation("Derive([)").is_err());
    }
}
