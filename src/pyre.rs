//! Whether Python's `re.compile` accepts a pattern, for pyxdg's
//! `checkRegex` (the `FilePattern` key). A port of the error checks in
//! CPython 3.14's `re._parser`, plus the look-behind width check of
//! `re._compiler`. Only validity is computed, no matcher is built.
//!
//! Not ported: `\N{name}` accepts any name made of the characters Unicode
//! names use (Python looks it up in its Unicode name table), and group names use Rust's notion of alphabetic
//! and alphanumeric characters for `str.isidentifier()`.

const MAXREPEAT: u128 = 4294967295;
const MAXGROUPS: usize = 1073741823;
const MAXCODE: u128 = 4294967295;
const MAXWIDTH: u128 = 1 << 64;

const FLAG_IGNORECASE: u8 = 1;
const FLAG_MULTILINE: u8 = 2;
const FLAG_DOTALL: u8 = 4;
const FLAG_VERBOSE: u8 = 8;
const FLAG_ASCII: u8 = 16;
const FLAG_UNICODE: u8 = 32;
const TYPE_FLAGS: u8 = FLAG_ASCII | FLAG_UNICODE;

struct Bad;

type Res<T> = Result<T, Bad>;

pub fn compiles(pattern: &str) -> bool {
    parse(pattern).is_ok()
}

/// One parsed item: what the repeat checks need and its width.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `^`, `$`, `\A`, `\b`, `\B`, `\z`, `\Z`: nothing to repeat.
    At,
    Repeat,
    Other,
}

#[derive(Clone, Copy)]
struct Item {
    kind: Kind,
    lo: u128,
    hi: u128,
}

impl Item {
    fn unit() -> Self {
        Self::other(1, 1)
    }

    fn other(lo: u128, hi: u128) -> Self {
        Item {
            kind: Kind::Other,
            lo,
            hi,
        }
    }
}

/// `SubPattern.getwidth()`.
fn width(items: &[Item]) -> (u128, u128) {
    let (lo, hi) = items.iter().fold((0u128, 0u128), |(lo, hi), i| {
        (lo.saturating_add(i.lo), hi.saturating_add(i.hi))
    });
    (lo.min(MAXWIDTH), hi.min(MAXWIDTH))
}

struct State {
    flags: u8,
    names: Vec<String>,
    name_ids: Vec<usize>,
    /// Width of each closed group; `None` while open. Index 0 is the whole
    /// pattern.
    group_widths: Vec<Option<(u128, u128)>>,
    lookbehind_groups: Option<usize>,
    /// Numbered conditional references, checked once all groups are known.
    cond_refs: Vec<usize>,
}

impl State {
    fn groups(&self) -> usize {
        self.group_widths.len()
    }

    fn open_group(&mut self, name: Option<String>) -> Res<usize> {
        let gid = self.groups();
        self.group_widths.push(None);
        if self.groups() > MAXGROUPS {
            return Err(Bad);
        }
        if let Some(name) = name {
            if self.names.contains(&name) {
                return Err(Bad);
            }
            self.names.push(name);
            self.name_ids.push(gid);
        }
        Ok(gid)
    }

    fn check_group(&self, gid: usize) -> bool {
        gid < self.groups() && self.group_widths[gid].is_some()
    }

    fn check_lookbehind_group(&self, gid: usize) -> Res<()> {
        if let Some(first) = self.lookbehind_groups
            && (!self.check_group(gid) || gid >= first)
        {
            return Err(Bad);
        }
        Ok(())
    }

    fn group_id(&self, name: &str) -> Option<usize> {
        let i = self.names.iter().position(|n| n == name)?;
        Some(self.name_ids[i])
    }
}

struct Source {
    chars: Vec<char>,
    index: usize,
    /// The next token: one character, or `\` and the character after it.
    next: Option<(char, Option<char>)>,
}

impl Source {
    fn new(pattern: &str) -> Res<Self> {
        let mut s = Source {
            chars: pattern.chars().collect(),
            index: 0,
            next: None,
        };
        s.advance()?;
        Ok(s)
    }

    fn advance(&mut self) -> Res<()> {
        let Some(&c) = self.chars.get(self.index) else {
            self.next = None;
            return Ok(());
        };
        if c == '\\' {
            let Some(&e) = self.chars.get(self.index + 1) else {
                return Err(Bad); // bad escape (end of pattern)
            };
            self.index += 2;
            self.next = Some(('\\', Some(e)));
        } else {
            self.index += 1;
            self.next = Some((c, None));
        }
        Ok(())
    }

    /// The next token as a plain character, `None` for escapes and the end.
    fn peek_char(&self) -> Option<char> {
        match self.next {
            Some((c, None)) => Some(c),
            _ => None,
        }
    }

    fn matches(&mut self, c: char) -> Res<bool> {
        if self.peek_char() == Some(c) {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn get(&mut self) -> Res<Option<(char, Option<char>)>> {
        let this = self.next;
        self.advance()?;
        Ok(this)
    }

    fn tell(&self) -> usize {
        match self.next {
            None => self.index,
            Some((_, None)) => self.index - 1,
            Some((_, Some(_))) => self.index - 2,
        }
    }

    fn seek(&mut self, index: usize) -> Res<()> {
        self.index = index;
        self.advance()
    }

    /// `getwhile`: up to `n` plain characters from `set`.
    fn get_while(&mut self, n: usize, set: impl Fn(char) -> bool) -> Res<String> {
        let mut out = String::new();
        for _ in 0..n {
            match self.peek_char() {
                Some(c) if set(c) => {
                    out.push(c);
                    self.advance()?;
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// `getuntil`: a non-empty name up to `terminator`. Escapes inside count
    /// as their two characters, as in Python.
    fn get_until(&mut self, terminator: char) -> Res<String> {
        let mut out = String::new();
        loop {
            let Some((c, e)) = self.get()? else {
                return Err(Bad);
            };
            if c == terminator && e.is_none() {
                if out.is_empty() {
                    return Err(Bad);
                }
                return Ok(out);
            }
            out.push(c);
            out.extend(e);
        }
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

fn hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn oct(c: char) -> bool {
    ('0'..='7').contains(&c)
}

/// `\N{name}` (see the module note) and `\x`, `\u`, `\U` escapes, shared by
/// both escape functions. `None` when `e` is none of them.
fn escape_literal(source: &mut Source, e: char) -> Option<Res<u32>> {
    let digits = match e {
        'x' => 2,
        'u' => 4,
        'U' => 8,
        'N' => {
            return Some((|| {
                if !source.matches('{')? {
                    return Err(Bad);
                }
                let name = source.get_until('}')?;
                // Unicode names and aliases use only these (case-insensitive)
                if !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
                {
                    return Err(Bad);
                }
                Ok(0)
            })());
        }
        _ => return None,
    };
    Some((|| {
        let h = source.get_while(digits, hex)?;
        if h.len() != digits {
            return Err(Bad);
        }
        let v = u32::from_str_radix(&h, 16).map_err(|_| Bad)?;
        if v > 0x10ffff {
            return Err(Bad); // chr() raises, reported as a bad escape
        }
        Ok(v)
    })())
}

enum ClassItem {
    Literal(u32),
    /// `\d`, `\s`, `\w` and their negations.
    Category,
}

/// `_class_escape`.
fn class_escape(source: &mut Source, e: char) -> Res<ClassItem> {
    match e {
        'a' | 'b' | 'f' | 'n' | 'r' | 't' | 'v' | '\\' => return Ok(ClassItem::Literal(0)),
        'd' | 'D' | 's' | 'S' | 'w' | 'W' => return Ok(ClassItem::Category),
        _ => {}
    }
    if let Some(v) = escape_literal(source, e) {
        return v.map(ClassItem::Literal);
    }
    if oct(e) {
        let rest = source.get_while(2, oct)?;
        let v = u32::from_str_radix(&format!("{e}{rest}"), 8).map_err(|_| Bad)?;
        if v > 0o377 {
            return Err(Bad);
        }
        return Ok(ClassItem::Literal(v));
    }
    if e.is_ascii_digit() || e.is_ascii_alphabetic() {
        return Err(Bad);
    }
    Ok(ClassItem::Literal(e as u32))
}

/// `_escape`, returning the parsed item.
fn escape(source: &mut Source, e: char, state: &State) -> Res<Item> {
    match e {
        'A' | 'b' | 'B' | 'z' | 'Z' => {
            return Ok(Item {
                kind: Kind::At,
                lo: 0,
                hi: 0,
            });
        }
        'd' | 'D' | 's' | 'S' | 'w' | 'W' => return Ok(Item::unit()),
        'a' | 'f' | 'n' | 'r' | 't' | 'v' | '\\' => return Ok(Item::unit()),
        _ => {}
    }
    if let Some(v) = escape_literal(source, e) {
        return v.map(|_| Item::unit());
    }
    if e == '0' {
        source.get_while(2, oct)?;
        return Ok(Item::unit());
    }
    if e.is_ascii_digit() {
        let mut digits = e.to_string();
        if let Some(d) = source.peek_char().filter(char::is_ascii_digit) {
            source.advance()?;
            digits.push(d);
            let first_two_octal = digits.chars().all(oct);
            if let Some(d3) = source.peek_char().filter(|&c| first_two_octal && oct(c)) {
                source.advance()?;
                digits.push(d3);
                let v = u32::from_str_radix(&digits, 8).map_err(|_| Bad)?;
                return if v > 0o377 {
                    Err(Bad)
                } else {
                    Ok(Item::unit())
                };
            }
        }
        let group: usize = digits.parse().map_err(|_| Bad)?;
        if group < state.groups() {
            if !state.check_group(group) {
                return Err(Bad);
            }
            state.check_lookbehind_group(group)?;
            let (lo, hi) = state.group_widths[group].unwrap_or((0, 0));
            return Ok(Item::other(lo, hi));
        }
        return Err(Bad);
    }
    if e.is_ascii_alphabetic() {
        return Err(Bad);
    }
    Ok(Item::unit())
}

fn parse(pattern: &str) -> Res<()> {
    let mut source = Source::new(pattern)?;
    let mut state = State {
        flags: 0,
        names: Vec::new(),
        name_ids: Vec::new(),
        group_widths: vec![None],
        lookbehind_groups: None,
        cond_refs: Vec::new(),
    };
    parse_sub(&mut source, &mut state, false, 0)?;
    // fix_flags: ASCII and UNICODE are incompatible
    if state.flags & FLAG_ASCII != 0 && state.flags & FLAG_UNICODE != 0 {
        return Err(Bad);
    }
    if source.next.is_some() {
        return Err(Bad); // unbalanced parenthesis
    }
    if state.cond_refs.iter().any(|&g| g >= state.groups()) {
        return Err(Bad);
    }
    Ok(())
}

/// `_parse_sub`: alternatives separated by `|`, returned as one subpattern.
fn parse_sub(
    source: &mut Source,
    state: &mut State,
    verbose: bool,
    nested: usize,
) -> Res<Vec<Item>> {
    let mut verbose = verbose;
    let mut alternatives: Vec<Vec<Item>> = Vec::new();
    loop {
        let first = nested == 0 && alternatives.is_empty();
        alternatives.push(parse_seq(source, state, verbose, nested + 1, first)?);
        if !source.matches('|')? {
            break;
        }
        if nested == 0 {
            verbose = state.flags & FLAG_VERBOSE != 0;
        }
    }
    if alternatives.len() == 1 {
        return Ok(alternatives.pop().unwrap_or_default());
    }
    let (lo, hi) = alternatives
        .iter()
        .map(|a| width(a))
        .fold((MAXWIDTH, 0), |(lo, hi), (l, h)| (lo.min(l), hi.max(h)));
    Ok(vec![Item::other(lo, hi)])
}

/// `_parse`: a sequence of items up to `|`, `)` or the end.
fn parse_seq(
    source: &mut Source,
    state: &mut State,
    verbose: bool,
    nested: usize,
    first: bool,
) -> Res<Vec<Item>> {
    let mut verbose = verbose;
    let mut items: Vec<Item> = Vec::new();
    while let Some((this, esc)) = source.next {
        if esc.is_none() && (this == '|' || this == ')') {
            break;
        }
        source.advance()?;

        if verbose && esc.is_none() {
            if " \t\n\r\u{b}\u{c}".contains(this) {
                continue;
            }
            if this == '#' {
                loop {
                    match source.get()? {
                        None | Some(('\n', None)) => break,
                        _ => {}
                    }
                }
                continue;
            }
        }

        if let Some(e) = esc {
            items.push(escape(source, e, state)?);
            continue;
        }
        match this {
            '[' => {
                parse_class(source)?;
                items.push(Item::unit());
            }
            '*' | '+' | '?' | '{' => {
                let here = source.tell();
                let (min, max) = match this {
                    '?' => (0, 1),
                    '*' => (0, MAXREPEAT),
                    '+' => (1, MAXREPEAT),
                    _ => {
                        if source.peek_char() == Some('}') {
                            items.push(Item::unit());
                            continue;
                        }
                        let lo = source.get_while(usize::MAX, |c| c.is_ascii_digit())?;
                        let hi = if source.matches(',')? {
                            source.get_while(usize::MAX, |c| c.is_ascii_digit())?
                        } else {
                            lo.clone()
                        };
                        if !source.matches('}')? {
                            items.push(Item::unit());
                            source.seek(here)?;
                            continue;
                        }
                        let mut min = 0;
                        let mut max = MAXREPEAT;
                        if !lo.is_empty() {
                            min = lo.parse::<u128>().unwrap_or(u128::MAX);
                            if min >= MAXREPEAT {
                                return Err(Bad);
                            }
                        }
                        if !hi.is_empty() {
                            max = hi.parse::<u128>().unwrap_or(u128::MAX);
                            if max >= MAXREPEAT || max < min {
                                return Err(Bad);
                            }
                        }
                        (min, max)
                    }
                };
                let Some(last) = items.last_mut() else {
                    return Err(Bad); // nothing to repeat
                };
                match last.kind {
                    Kind::At => return Err(Bad),
                    Kind::Repeat => return Err(Bad), // multiple repeat
                    Kind::Other => {}
                }
                // lazy `?` or possessive `+`
                if !source.matches('?')? {
                    source.matches('+')?;
                }
                let (i, j) = (last.lo, last.hi);
                last.kind = Kind::Repeat;
                last.lo = i.saturating_mul(min);
                last.hi = if max == MAXREPEAT && j > 0 {
                    MAXWIDTH
                } else {
                    j.saturating_mul(max)
                };
            }
            '.' => items.push(Item::unit()),
            '(' => {
                if let Some(item) =
                    parse_group(source, state, &mut verbose, nested, first, items.is_empty())?
                {
                    items.push(item);
                }
            }
            '^' | '$' => items.push(Item {
                kind: Kind::At,
                lo: 0,
                hi: 0,
            }),
            _ => items.push(Item::unit()),
        }
    }
    Ok(items)
}

/// A character class after its `[`.
fn parse_class(source: &mut Source) -> Res<()> {
    source.matches('^')?;
    let mut any = false;
    loop {
        let Some((this, esc)) = source.get()? else {
            return Err(Bad); // unterminated character set
        };
        if this == ']' && esc.is_none() && any {
            return Ok(());
        }
        any = true;
        let code1 = match esc {
            Some(e) => class_escape(source, e)?,
            None => ClassItem::Literal(this as u32),
        };
        if source.matches('-')? {
            let Some((that, esc2)) = source.get()? else {
                return Err(Bad);
            };
            if that == ']' && esc2.is_none() {
                return Ok(());
            }
            let code2 = match esc2 {
                Some(e) => class_escape(source, e)?,
                None => ClassItem::Literal(that as u32),
            };
            let lo = match code1 {
                ClassItem::Literal(v) => literal_value(v, this, esc),
                ClassItem::Category => return Err(Bad),
            };
            let hi = match code2 {
                ClassItem::Literal(v) => literal_value(v, that, esc2),
                ClassItem::Category => return Err(Bad),
            };
            if hi < lo {
                return Err(Bad); // bad character range
            }
        }
    }
}

/// The code point of a class literal: `class_escape` returns 0 for the
/// fixed escapes, so resolve those here.
fn literal_value(v: u32, c: char, esc: Option<char>) -> u32 {
    match esc {
        None => c as u32,
        Some('a') => 7,
        Some('b') => 8,
        Some('f') => 12,
        Some('n') => 10,
        Some('r') => 13,
        Some('t') => 9,
        Some('v') => 11,
        Some('\\') => '\\' as u32,
        Some(_) => v,
    }
}

/// A group after its `(`. `None` for groups that add no item: comments and
/// global flags.
fn parse_group(
    source: &mut Source,
    state: &mut State,
    verbose: &mut bool,
    nested: usize,
    first: bool,
    seq_empty: bool,
) -> Res<Option<Item>> {
    let mut capture = true;
    let mut name = None;
    let mut add_flags = 0;
    let mut del_flags = 0;
    if source.matches('?')? {
        let Some((c, esc)) = source.get()? else {
            return Err(Bad);
        };
        if esc.is_some() {
            return Err(Bad); // unknown extension
        }
        match c {
            'P' => {
                if source.matches('<')? {
                    let n = source.get_until('>')?;
                    if !is_identifier(&n) {
                        return Err(Bad);
                    }
                    name = Some(n);
                } else if source.matches('=')? {
                    let n = source.get_until(')')?;
                    if !is_identifier(&n) {
                        return Err(Bad);
                    }
                    let gid = state.group_id(&n).ok_or(Bad)?;
                    if !state.check_group(gid) {
                        return Err(Bad);
                    }
                    state.check_lookbehind_group(gid)?;
                    let (lo, hi) = state.group_widths[gid].unwrap_or((0, 0));
                    return Ok(Some(Item::other(lo, hi)));
                } else {
                    return Err(Bad);
                }
            }
            ':' => capture = false,
            '#' => loop {
                match source.get()? {
                    None => return Err(Bad), // unterminated comment
                    Some((')', None)) => return Ok(None),
                    _ => {}
                }
            },
            '=' | '!' | '<' => {
                let mut kind = c;
                let behind = c == '<';
                let outer = state.lookbehind_groups;
                if behind {
                    match source.get()? {
                        Some((k @ ('=' | '!'), None)) => kind = k,
                        _ => return Err(Bad),
                    }
                    if outer.is_none() {
                        state.lookbehind_groups = Some(state.groups());
                    }
                }
                let p = parse_sub(source, state, *verbose, nested + 1)?;
                if behind && outer.is_none() {
                    state.lookbehind_groups = None;
                }
                if !source.matches(')')? {
                    return Err(Bad);
                }
                // an empty negative assertion becomes FAILURE and is not
                // compiled, so its width is never checked
                if behind && (kind == '=' || !p.is_empty()) {
                    let (lo, hi) = width(&p);
                    if lo > MAXCODE || lo != hi {
                        return Err(Bad); // look-behind requires fixed-width pattern
                    }
                }
                return Ok(Some(Item::other(0, 0)));
            }
            '(' => {
                let cond = source.get_until(')')?;
                let gid = if cond.chars().all(|c| c.is_ascii_digit()) {
                    let g: usize = cond.parse().unwrap_or(usize::MAX);
                    if g == 0 || g >= MAXGROUPS {
                        return Err(Bad);
                    }
                    state.cond_refs.push(g);
                    g
                } else {
                    if !is_identifier(&cond) {
                        return Err(Bad);
                    }
                    state.group_id(&cond).ok_or(Bad)?
                };
                state.check_lookbehind_group(gid)?;
                let yes = parse_seq(source, state, *verbose, nested + 1, false)?;
                let (mut lo, mut hi) = width(&yes);
                if source.matches('|')? {
                    let no = parse_seq(source, state, *verbose, nested + 1, false)?;
                    if source.peek_char() == Some('|') {
                        return Err(Bad); // more than two branches
                    }
                    let (l, h) = width(&no);
                    lo = lo.min(l);
                    hi = hi.max(h);
                } else {
                    lo = 0;
                }
                if !source.matches(')')? {
                    return Err(Bad);
                }
                return Ok(Some(Item::other(lo, hi)));
            }
            '>' => capture = false,
            'i' | 'L' | 'm' | 's' | 'x' | 'a' | 'u' | '-' => match parse_flags(source, c)? {
                Flags::Global(add) => {
                    state.flags |= add;
                    if !first || !seq_empty {
                        return Err(Bad); // global flags not at the start
                    }
                    *verbose = state.flags & FLAG_VERBOSE != 0;
                    return Ok(None);
                }
                Flags::Scoped(add, del) => {
                    add_flags = add;
                    del_flags = del;
                    capture = false;
                }
            },
            _ => return Err(Bad), // unknown extension
        }
    }
    let group = if capture {
        Some(state.open_group(name)?)
    } else {
        None
    };
    let sub_verbose = (*verbose || add_flags & FLAG_VERBOSE != 0) && del_flags & FLAG_VERBOSE == 0;
    let p = parse_sub(source, state, sub_verbose, nested + 1)?;
    if !source.matches(')')? {
        return Err(Bad);
    }
    let (lo, hi) = width(&p);
    if let Some(g) = group {
        state.group_widths[g] = Some((lo, hi));
    }
    Ok(Some(Item::other(lo, hi)))
}

enum Flags {
    /// `(?aimsux)`: applies to the whole pattern.
    Global(u8),
    /// `(?flags-flags:...)`: added and removed for the group.
    Scoped(u8, u8),
}

/// The flag bit for `c`, `None` if it is not a flag. `L` is a flag but is
/// rejected for str patterns, which is all pyxdg compiles.
fn flag(c: char) -> Option<Res<u8>> {
    Some(Ok(match c {
        'i' => FLAG_IGNORECASE,
        'm' => FLAG_MULTILINE,
        's' => FLAG_DOTALL,
        'x' => FLAG_VERBOSE,
        'a' => FLAG_ASCII,
        'u' => FLAG_UNICODE,
        'L' => return Some(Err(Bad)),
        _ => return None,
    }))
}

/// A plain character token, `None` for an escape or the end.
fn next_plain(source: &mut Source) -> Res<Option<char>> {
    Ok(match source.get()? {
        Some((c, None)) => Some(c),
        _ => None,
    })
}

/// `_parse_flags`, after `(?` and the first flag character `c`.
fn parse_flags(source: &mut Source, mut c: char) -> Res<Flags> {
    let mut add = 0;
    let mut del = 0;
    if c != '-' {
        loop {
            let f = flag(c).ok_or(Bad)??;
            add |= f;
            if f & TYPE_FLAGS != 0 && add & TYPE_FLAGS != f {
                return Err(Bad); // 'a', 'u' and 'L' are incompatible
            }
            c = next_plain(source)?.ok_or(Bad)?;
            if ")-:".contains(c) {
                break;
            }
            if flag(c).is_none() {
                return Err(Bad);
            }
        }
    }
    if c == ')' {
        return Ok(Flags::Global(add));
    }
    if c == '-' {
        c = next_plain(source)?.ok_or(Bad)?;
        loop {
            let f = flag(c).ok_or(Bad)??;
            if f & TYPE_FLAGS != 0 {
                return Err(Bad); // cannot turn off 'a', 'u' and 'L'
            }
            del |= f;
            c = next_plain(source)?.ok_or(Bad)?;
            if c == ':' {
                break;
            }
            if flag(c).is_none() {
                return Err(Bad);
            }
        }
    }
    if add & del != 0 {
        return Err(Bad); // flag turned on and off
    }
    Ok(Flags::Scoped(add, del))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(h: &str) -> String {
        let bytes: Vec<u8> = (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    /// Answers recorded from Python 3.14's `re.compile` by
    /// `tools/gen-pyre-corpus.py`. Set `RWSM_PYRE_CORPUS` to check a larger
    /// generated file.
    #[test]
    fn matches_python_re() {
        let text = match std::env::var("RWSM_PYRE_CORPUS") {
            Ok(path) => std::fs::read_to_string(path).unwrap(),
            Err(_) => include_str!("testdata/pyre_corpus.txt").to_string(),
        };
        let mut wrong = Vec::new();
        let mut total = 0;
        for line in text.lines() {
            let (ok, h) = line.split_once('\t').unwrap();
            let pattern = unhex(h);
            total += 1;
            if compiles(&pattern) != (ok == "1") {
                wrong.push(format!("{ok} {pattern:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {total} differ:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
}
