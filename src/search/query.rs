//! Taking one search line apart into a term, a match mode and a type filter.
//!
//! # Why the syntax lives in the line
//!
//! The alternative was a set of toggles held beside the query. A toggle is
//! state that lives somewhere other than where the person is looking: it
//! survives a history recall it was never part of, it needs room on a footer
//! that is already short of it, and "why is this list so short" becomes a
//! question about a chip nobody noticed. Syntax in the line is on screen by
//! construction, is recalled with the line, is copied with the line, and
//! needs no footer at all. F3 and F4 do not introduce a second source of
//! truth - they edit the line, so what will run is always what is shown.
//!
//! # Why only characters Windows forbids in a name
//!
//! Windows refuses `" * ? < > | : \ /` in a filename, so syntax spelled with
//! them can never be ambiguous with the thing being searched for. Only `*`
//! and `:` are used here, and neither can occur in any name this program will
//! ever index.
//!
//! `^` and `$` were considered for the anchors and rejected on exactly that
//! test: both are legal in a name, so `^draft` could not be searched for
//! literally - the query would quietly become a prefix search instead. That
//! is the class of silent wrongness this crate is built against.
//!
//! A whole-stem `"..."` form was considered and dropped along with the mode it
//! spelled. Quotes fail twice over: everywhere else they mean "take this
//! literally", so `"11-D-0704"` would return nothing where the person meant
//! everything; and they are a *paired* delimiter in a box that searches on
//! every keystroke, so the line sits in the unusable state `"11-D-070` for as
//! long as it takes to type the term. Every token here is instead a valid
//! query at every keystroke on the way to it.
//!
//! # Why a bare `.pdf` is not a type filter
//!
//! `.pdf` is a legal substring of a name - `report .pdf.bak` is a real file -
//! so reading it as syntax would make searching for it impossible. `ext:`
//! cannot occur in a name at all, which is the entire point.
//!
//! # Why the term is never split on whitespace
//!
//! Filenames on these shares are full of spaces: `11-D-0704 survey.pdf`,
//! `0704 big`. `11-D-0704 survey` is a good query today and has to stay one.
//! So the term is the line minus the byte ranges of the filter tokens found
//! in it, and nothing else - never a list of words.
//!
//! # Why parsing cannot fail
//!
//! The line is re-parsed on every keystroke, and half the states it passes
//! through on the way to being finished are states no parser can honour:
//! `ext:` with nothing after it, a `*` that is about to be the start of
//! something. A parse returning an error instead of a query would be a
//! results pane going blank while somebody is still typing into it. So
//! [`Query::parse`] is total, and anything it could not honour rides along on
//! the result as a [`Problem`].

use crate::config::MIN_QUERY_LEN;
use crate::config::hidden::Hidden;
use crate::util::fold;

/// Where the term has to sit within the name.
///
/// The anchored forms are about the *stem* - the name without its final
/// extension - because the extension is what [`TypeFilter`] is for. Anchoring
/// on the whole name instead would make [`Self::Suffix`] useless: almost every
/// name ends in `.pdf` or `.dwg`, not in anything anybody would type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MatchMode {
    /// `report`, or `*report*`. Anywhere in the name.
    #[default]
    Contains,
    /// `report*`. The name starts with it.
    Prefix,
    /// `*report`. The stem ends with it.
    Suffix,
}

impl MatchMode {
    /// Every mode, in the order F3 cycles them.
    pub const ALL: [Self; 3] = [Self::Contains, Self::Prefix, Self::Suffix];

    /// How this reads in a sentence, for the empty-results pane.
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "anywhere in the name",
            Self::Prefix => "at the start of the name",
            Self::Suffix => "at the end of the name",
        }
    }

    /// The next mode, for F3.
    pub fn next(self) -> Self {
        match self {
            Self::Contains => Self::Prefix,
            Self::Prefix => Self::Suffix,
            Self::Suffix => Self::Contains,
        }
    }

    /// True when a candidate's stem has to be located to judge it.
    #[inline]
    pub fn needs_stem(self) -> bool {
        matches!(self, Self::Suffix)
    }
}

/// Longest extension a filter will hold.
///
/// `sldprt` and `jpeg` fit with room over. Anything longer is not something
/// anyone narrows by, and a fixed bound is what lets an extension sit inline
/// with no allocation anywhere on the match path.
pub const MAX_EXT_LEN: usize = 8;

/// One extension a query admits: folded, and without its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext {
    bytes: [u8; MAX_EXT_LEN],
    len: u8,
}

impl Ext {
    /// `None` for anything no file can have as an extension.
    ///
    /// One leading dot is accepted and dropped, so `ext:.pdf` and `ext:pdf`
    /// ask the same thing - which is what anyone who thinks of an extension as
    /// `.pdf` will type.
    ///
    /// Non-ASCII is refused for the reason [`Hidden::new`] refuses it: the
    /// arena is folded by [`crate::util::fold`], which declines the folds that
    /// would change a byte length, so a non-ASCII extension would compare one
    /// way against the arena and another against an original name.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('.').unwrap_or(s);
        if s.is_empty() || s.len() > MAX_EXT_LEN || !s.is_ascii() {
            return None;
        }
        // Windows forbids these in a name, and a dot would mean the caller
        // split on the wrong thing. Either way it could never match.
        if s.contains(['"', '*', '?', '<', '>', '|', ':', '\\', '/', '.']) {
            return None;
        }
        let mut bytes = [0u8; MAX_EXT_LEN];
        bytes[..s.len()].copy_from_slice(s.to_ascii_lowercase().as_bytes());
        Some(Self {
            bytes,
            len: s.len() as u8,
        })
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// As it is written in the search line, without the dot.
    pub fn as_str(&self) -> &str {
        // Every byte came from an ASCII string in `parse`.
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

/// The extensions a query admits.
///
/// Empty means *every* extension, not *no* extension. That asymmetry is what
/// lets `ext:` with nothing after it yet - which exists for as long as it
/// takes to type `pdf` - mean "no filter" rather than emptying the pane
/// mid-word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeFilter {
    /// `None` rather than an empty list, and behind a *thin* pointer.
    ///
    /// Every `AppEvent` now carries a [`Query`], and `AppEvent` travels
    /// through a bounded channel the whole application shares, so anything
    /// stored inline here is paid for by every event - including the ones that
    /// hold no query at all. An unfiltered query is overwhelmingly the common
    /// case, and this way it costs eight bytes and a null check.
    ///
    /// `Box<Vec<_>>` rather than `Box<[_]>` because the latter is a *fat*
    /// pointer: sixteen bytes, which is enough to push `VerifyMsg` over the
    /// size at which the whole event becomes worth boxing. The extra
    /// indirection is read once per candidate only when a filter is present,
    /// and `is_none` short-circuits before it when one is not.
    // The thin pointer is the point; see above. Clippy reads this shape as an
    // accidental double indirection, which it usually is.
    #[allow(clippy::box_collection)]
    exts: Option<Box<Vec<Ext>>>,
}

impl TypeFilter {
    /// Builds a filter from extension names, dropping the ones no file could
    /// have.
    ///
    /// Duplicates are removed: one costs a comparison per candidate for
    /// nothing, and a hand-typed list is exactly where one turns up.
    pub fn new<S: AsRef<str>>(names: &[S]) -> Self {
        let mut exts: Vec<Ext> = Vec::new();
        for n in names {
            if let Some(e) = Ext::parse(n.as_ref())
                && !exts.contains(&e)
            {
                exts.push(e);
            }
        }
        Self::from_vec(exts)
    }

    fn from_vec(exts: Vec<Ext>) -> Self {
        Self {
            exts: (!exts.is_empty()).then(|| Box::new(exts)),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.exts.is_none()
    }

    pub fn len(&self) -> usize {
        self.exts.as_ref().map_or(0, |e| e.len())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Ext> {
        self.exts.iter().flat_map(|e| e.iter())
    }

    /// Whether a folded name whose extension starts at `ext_start` is one of
    /// these types.
    ///
    /// `ext_start` is the name's length for a name with no dot in it, which
    /// yields an empty extension - and no [`Ext`] is ever empty, so a dotless
    /// name is refused by any non-empty filter. That is right: somebody who
    /// asked for PDFs did not ask for `README`.
    #[inline]
    pub fn admits(&self, name: &[u8], ext_start: usize) -> bool {
        let Some(exts) = &self.exts else {
            return true;
        };
        let ext = &name[ext_start..];
        exts.iter().any(|e| e.as_bytes() == ext)
    }

    /// The same question of a name whose stem has not been located yet.
    #[inline]
    pub fn admits_name(&self, name: &[u8]) -> bool {
        if self.exts.is_none() {
            return true;
        }
        self.admits(name, split_stem(name).1)
    }
}

/// The end of the stem, and where the extension starts.
///
/// The *last* dot splits, uniformly: `11-D-0704.rev2.pdf` has the stem
/// `11-D-0704.rev2`. `README` has an empty extension and is therefore refused
/// by every type filter, which is right. `.gitignore` has an empty *stem* and
/// so can never be a suffix match - a consequence of the rule rather than a
/// policy, and pinned by a test so it stays deliberate.
#[inline]
pub fn split_stem(name: &[u8]) -> (usize, usize) {
    match memchr::memrchr(b'.', name) {
        Some(dot) => (dot, dot + 1),
        None => (name.len(), name.len()),
    }
}

/// Something in the line the parser could not honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// A `*` somewhere other than the very start or the very end.
    ///
    /// Refused rather than read as literal text. `*` cannot occur in a Windows
    /// filename, so a literal reading is a guaranteed empty list with nothing
    /// on screen to explain it - which looks exactly like the file having gone
    /// missing.
    InteriorStar,
    /// `ext:` naming something no file can have as an extension.
    BadExtension,
}

impl Problem {
    pub fn detail(self) -> &'static str {
        match self {
            Self::InteriorStar => "a * only works at the start or the end",
            Self::BadExtension => "that is not a file extension",
        }
    }
}

/// Why a query was not run at all.
///
/// Lives here rather than in [`crate::search::matcher`] because every variant
/// is a property of what was typed rather than of any index - which is the
/// same reason the judgement is made once for the whole search instead of once
/// per share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryReject {
    /// The *term* is below [`MIN_QUERY_LEN`]. Not the line: `ab ext:pdf` is a
    /// ten-character line and a two-character sweep across every name on the
    /// share, which is the work the minimum exists to prevent.
    TooShort { need: usize },
    /// Contains a NUL byte.
    ///
    /// Rejected rather than stripped: stripping would let `a\0bc` match `abc`,
    /// which is silently wrong. This check is also what upholds the guarantee
    /// that a match can never span two arena entries.
    ContainsNul,
    /// The line holds syntax that could not be honoured.
    Syntax(Problem),
}

impl QueryReject {
    pub fn detail(&self) -> String {
        match self {
            Self::TooShort { need } => format!("type at least {need} characters"),
            Self::ContainsNul => "that is not a code this can look for".into(),
            Self::Syntax(p) => p.detail().into(),
        }
    }
}

/// The token that introduces a type filter.
const EXT_PREFIX: &str = "ext:";

/// The types F4 cycles through, in order.
///
/// Two, because these shares hold drawings: a job is a PDF document or a DWG
/// drawing, and everything else on the share is something somebody put beside
/// them. Anything outside this list is still reachable by typing `ext:` and
/// the extension, which is the point of the syntax being in the line - a key
/// that cycles a short list and a line that can say anything are not in
/// competition.
pub const CYCLE_EXTENSIONS: &[&str] = &["pdf", "dwg"];

/// One search line, taken apart.
///
/// The term is as literal as the line it came from: the parser only ever
/// recognises syntax spelled with characters Windows forbids in a name, and
/// hands everything else through untouched. [`crate::search::pattern`] leans
/// on that when it decides whether a query may be given to the file server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// `Box<str>`, not `String`: a parsed query is never appended to, and the
    /// eight bytes of spare capacity a `String` carries are eight bytes on
    /// every `AppEvent` in the channel.
    term: Box<str>,
    mode: MatchMode,
    types: TypeFilter,
    problem: Option<Problem>,
}

impl Query {
    /// A plain substring search, for the callers that only ever wanted one.
    pub fn contains(term: impl AsRef<str>) -> Self {
        Self {
            term: term.as_ref().into(),
            ..Self::default()
        }
    }

    /// Takes a search line apart. Total: see the module note.
    pub fn parse(line: &str) -> Self {
        let mut exts: Vec<Ext> = Vec::new();
        let mut problem = None;
        let mut term = String::with_capacity(line.len());

        for token in line.split_ascii_whitespace() {
            let Some(value) = strip_ext_prefix(token) else {
                if !term.is_empty() {
                    term.push(' ');
                }
                term.push_str(token);
                continue;
            };
            // Skipping empty pieces is what makes `ext:`, `ext:,` and
            // `ext:pdf,` all mean the same thing as the filter they are on the
            // way to becoming.
            for piece in value.split(',').filter(|p| !p.trim().is_empty()) {
                match Ext::parse(piece) {
                    Some(e) if !exts.contains(&e) => exts.push(e),
                    Some(_) => {}
                    None => problem = Some(Problem::BadExtension),
                }
            }
        }

        // Anchors, stripped one at a time so `*report*` asks the same thing as
        // `report` rather than a third thing.
        let mut rest = term.as_str();
        let lead = rest.starts_with('*');
        if lead {
            rest = &rest[1..];
        }
        let trail = rest.ends_with('*');
        if trail {
            rest = &rest[..rest.len() - 1];
        }
        let mode = match (lead, trail) {
            (true, false) => MatchMode::Suffix,
            (false, true) => MatchMode::Prefix,
            _ => MatchMode::Contains,
        };
        if rest.contains('*') {
            problem = Some(Problem::InteriorStar);
        }

        Self {
            term: rest.into(),
            mode,
            types: TypeFilter::from_vec(exts),
            problem,
        }
    }

    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn mode(&self) -> MatchMode {
        self.mode
    }

    pub fn types(&self) -> &TypeFilter {
        &self.types
    }

    pub fn problem(&self) -> Option<Problem> {
        self.problem
    }

    /// Whether anything beyond a plain substring search was asked for.
    ///
    /// The sweep branches on this once, so that an unfiltered search - which
    /// is every search until somebody presses F3 - does exactly the work it
    /// did before any of this existed.
    #[inline]
    pub fn is_filtered(&self) -> bool {
        self.mode != MatchMode::Contains || !self.types.is_empty()
    }

    /// Judges the query without consulting any index.
    pub fn check(&self) -> Result<(), QueryReject> {
        if let Some(p) = self.problem {
            return Err(QueryReject::Syntax(p));
        }
        if self.term.chars().count() < MIN_QUERY_LEN {
            return Err(QueryReject::TooShort {
                need: MIN_QUERY_LEN,
            });
        }
        if fold::fold_query(&self.term).contains(&0) {
            return Err(QueryReject::ContainsNul);
        }
        Ok(())
    }

    /// Whether this is worth dispatching at all.
    pub fn is_searchable(&self) -> bool {
        self.check().is_ok()
    }

    /// The same query with a different mode, for F3.
    pub fn with_mode(&self, mode: MatchMode) -> Self {
        Self {
            mode,
            ..self.clone()
        }
    }

    /// The same query admitting different types, for F4.
    pub fn with_types(&self, types: TypeFilter) -> Self {
        Self {
            types,
            ..self.clone()
        }
    }

    /// The next step of F4: no filter, then each of [`CYCLE_EXTENSIONS`], then
    /// no filter again.
    ///
    /// A filter the key cannot have produced - anything typed, or more than
    /// one extension - cycles back to no filter rather than to the next entry.
    /// Guessing where somebody's hand-typed `ext:sldprt` sits in a list that
    /// does not contain it would be inventing an answer; clearing it is the
    /// one step that is always what it looks like.
    pub fn next_types(&self) -> TypeFilter {
        let at = match self.types.len() {
            0 => 0,
            1 => {
                let current = self.types.iter().next().map(Ext::as_str).unwrap_or("");
                match CYCLE_EXTENSIONS.iter().position(|e| *e == current) {
                    Some(i) => i + 1,
                    None => CYCLE_EXTENSIONS.len(),
                }
            }
            _ => CYCLE_EXTENSIONS.len(),
        };
        match CYCLE_EXTENSIONS.get(at) {
            Some(e) => TypeFilter::new(&[e]),
            None => TypeFilter::default(),
        }
    }

    /// This query written back out as a search line.
    ///
    /// Round-trips through [`Self::parse`]. That is what lets F3 and F4 edit
    /// the line rather than keeping a second copy of the filter beside it: the
    /// line stays the only place the query lives.
    pub fn to_line(&self) -> String {
        let mut out = String::with_capacity(self.term.len() + 16);
        if self.mode == MatchMode::Suffix {
            out.push('*');
        }
        out.push_str(&self.term);
        if self.mode == MatchMode::Prefix {
            out.push('*');
        }
        if !self.types.is_empty() {
            out.push(' ');
            out.push_str(EXT_PREFIX);
            for (i, e) in self.types.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(e.as_str());
            }
        }
        out
    }

    /// How the narrowing reads in a sentence, for the empty-results pane.
    ///
    /// `None` when nothing was narrowed, so the caller adds no line at all
    /// rather than one saying everything was looked at.
    pub fn describe(&self) -> Option<String> {
        let types: Vec<&str> = self.types.iter().map(Ext::as_str).collect();
        match (self.mode, types.as_slice()) {
            (MatchMode::Contains, []) => None,
            (MatchMode::Contains, t) => Some(format!("Only {} files were looked at.", join(t))),
            (m, []) => Some(format!("Only names with it {} were looked at.", m.label())),
            (m, t) => Some(format!(
                "Only {} files with it {} were looked at.",
                join(t),
                m.label()
            )),
        }
    }
}

/// `ext:` at the head of a token, whatever its case. `None` when the token is
/// ordinary text.
///
/// Matched as a whole token rather than anywhere in the line, so `context:`
/// and `next:pdf` stay part of the term - they are things somebody could
/// reasonably be searching for.
fn strip_ext_prefix(token: &str) -> Option<&str> {
    let head = token.get(..EXT_PREFIX.len())?;
    head.eq_ignore_ascii_case(EXT_PREFIX)
        .then(|| &token[EXT_PREFIX.len()..])
}

/// `.pdf and .dwg`, for a sentence.
fn join(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => format!(".{one}"),
        [rest @ .., last] => {
            let head: Vec<String> = rest.iter().map(|r| format!(".{r}")).collect();
            format!("{} and .{}", head.join(", "), last)
        }
    }
}

/// Whether a folded name satisfies a query's mode and type filter, and if so
/// which occurrence of the needle justified it.
///
/// A free function rather than only a [`Sieve`] method because three callers
/// need exactly this question and none of them may answer it differently: the
/// sweep, [`crate::search::verify`]'s audit oracle, and
/// [`crate::search::pattern::confirms`], which narrows a deliberately loose
/// server answer back to what was asked for. A second implementation is how
/// the audit comes to disagree with the index it is auditing, and three
/// disagreements switch server-side filtering off for the rest of the process.
///
/// `pos` is the *leftmost* occurrence of `needle` in `name`. See
/// [`Sieve::admits`] for why the trailing form does not trust it.
#[inline]
pub fn admits(
    name: &[u8],
    needle: &[u8],
    pos: u32,
    mode: MatchMode,
    types: &TypeFilter,
) -> Option<u32> {
    match mode {
        MatchMode::Contains => types.admits_name(name).then_some(pos),
        MatchMode::Prefix => (pos == 0 && types.admits_name(name)).then_some(0),
        MatchMode::Suffix => {
            let (stem_end, ext_start) = split_stem(name);
            if !types.admits(name, ext_start) {
                return None;
            }
            let n = needle.len();
            (stem_end >= n && &name[stem_end - n..stem_end] == needle)
                .then(|| (stem_end - n) as u32)
        }
    }
}

/// Everything a candidate row must satisfy beyond holding the needle.
///
/// One type rather than a hidden-list parameter beside a query parameter,
/// because the sweep asks one question per candidate and every answer should
/// come from one place. Two predicates threaded separately is how the two come
/// to disagree.
pub struct Sieve<'a> {
    hidden: &'a Hidden,
    needle: &'a [u8],
    mode: MatchMode,
    types: &'a TypeFilter,
}

impl<'a> Sieve<'a> {
    pub fn new(hidden: &'a Hidden, needle: &'a [u8], query: &'a Query) -> Self {
        Self {
            hidden,
            needle,
            mode: query.mode(),
            types: query.types(),
        }
    }

    /// Whether `name` is admitted, and if so which occurrence of the needle
    /// justified it.
    ///
    /// `name` is one entry's folded bytes without its separator, and `pos` is
    /// the leftmost occurrence of the needle in it, which the sweep already
    /// holds.
    ///
    /// # Why the leftmost occurrence is not the one tested
    ///
    /// The sweep records only the leftmost occurrence per entry: on the first
    /// hit the cursor jumps past the whole entry, which is what makes
    /// `matched` count entries rather than hits. For a prefix that is exactly
    /// the right occurrence - if any occurrence sits at zero then the leftmost
    /// one does. For a suffix it is the wrong one. `report_report.pdf` matches
    /// `report` at offset zero first, and testing *that* against the end of
    /// the stem would drop a file whose stem does end in `report`: a false
    /// negative with no symptom on screen at all. So the suffix form re-checks
    /// the stem's own tail instead of whichever occurrence the sweep happened
    /// to stop at.
    #[inline]
    pub fn admits(&self, name: &[u8], pos: u32) -> Option<u32> {
        if self.hidden.hides_folded(name) {
            return None;
        }
        admits(name, self.needle, pos, self.mode, self.types)
    }

    /// The same question asked on the slow, fully Unicode-correct path.
    ///
    /// That path lowercases each name into a *separate* string rather than
    /// reading the folded arena, and `to_lowercase` is not byte-length
    /// preserving - which is the whole reason it exists. So the offsets the
    /// fast path works in do not apply, and both the stem and the needle have
    /// to be found again in `lower`.
    ///
    /// `name` is the original, for [`Hidden::hides`], which folds ASCII case
    /// itself. The type filter is compared against `lower`, so an extension
    /// that is not ASCII is not matched here - the same conservative refusal
    /// [`Ext::parse`] already makes, and harmless because this path only runs
    /// when the fast one found nothing at all.
    #[inline]
    pub fn admits_lossy(&self, name: &str, lower: &str, needle: &str, pos: u32) -> Option<u32> {
        if self.hidden.hides(name) {
            return None;
        }
        let bytes = lower.as_bytes();
        let (stem_end, ext_start) = split_stem(bytes);
        if !self.types.admits(bytes, ext_start) {
            return None;
        }
        match self.mode {
            MatchMode::Contains => Some(pos),
            MatchMode::Prefix => (pos == 0).then_some(0),
            MatchMode::Suffix => {
                let n = needle.len();
                (stem_end >= n && &bytes[stem_end - n..stem_end] == needle.as_bytes())
                    .then(|| (stem_end - n) as u32)
            }
        }
    }

    /// Whether a file pulled in by its *folder* matching is admitted.
    ///
    /// The mode is deliberately not applied: this file's own name did not
    /// match and was never expected to. Testing it would empty the folder that
    /// is the whole reason the row is there.
    #[inline]
    pub fn admits_inherited(&self, name: &[u8]) -> bool {
        !self.hidden.hides_folded(name) && self.types.admits_name(name)
    }

    /// Whether a *folder* name satisfies the mode.
    ///
    /// A folder has no extension, so the type filter does not apply to it and
    /// the whole name is its stem. Splitting `11.5 revisions` on its dot would
    /// be nonsense.
    #[inline]
    pub fn admits_folder(&self, name: &[u8], pos: u32) -> Option<u32> {
        match self.mode {
            MatchMode::Contains => Some(pos),
            MatchMode::Prefix => (pos == 0).then_some(0),
            MatchMode::Suffix => {
                let n = self.needle.len();
                (name.len() >= n && &name[name.len() - n..] == self.needle)
                    .then(|| (name.len() - n) as u32)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sieve<'a>(hidden: &'a Hidden, needle: &'a [u8], q: &'a Query) -> Sieve<'a> {
        Sieve::new(hidden, needle, q)
    }

    // --- the shape of a line ------------------------------------------------

    #[test]
    fn a_bare_term_is_a_contains_search() {
        let q = Query::parse("report");
        assert_eq!(q.term(), "report");
        assert_eq!(q.mode(), MatchMode::Contains);
        assert!(q.types().is_empty());
        assert!(!q.is_filtered());
    }

    #[test]
    fn a_trailing_star_anchors_the_term_to_the_start_of_the_name() {
        let q = Query::parse("report*");
        assert_eq!(q.term(), "report");
        assert_eq!(q.mode(), MatchMode::Prefix);
    }

    #[test]
    fn a_leading_star_anchors_the_term_to_the_end_of_the_stem() {
        let q = Query::parse("*report");
        assert_eq!(q.term(), "report");
        assert_eq!(q.mode(), MatchMode::Suffix);
    }

    #[test]
    fn stars_on_both_sides_ask_the_same_thing_as_neither() {
        assert_eq!(Query::parse("*report*"), Query::parse("report"));
    }

    /// `*` cannot occur in a Windows filename, so reading it literally is a
    /// guaranteed empty list with nothing on screen to explain it - which is
    /// indistinguishable from the file having gone missing.
    #[test]
    fn a_star_in_the_middle_is_refused_rather_than_searched_for_literally() {
        let q = Query::parse("re*port");
        assert_eq!(q.problem(), Some(Problem::InteriorStar));
        assert_eq!(q.check(), Err(QueryReject::Syntax(Problem::InteriorStar)));
    }

    /// Filenames on these shares are full of spaces - `11-D-0704 survey.pdf`
    /// is one of them. A parser that split the line into words would turn that
    /// query into two terms and find nothing.
    #[test]
    fn a_term_with_spaces_in_it_survives_the_filter_being_lifted_out() {
        let q = Query::parse("11-D-0704 survey ext:pdf");
        assert_eq!(q.term(), "11-D-0704 survey");
        assert_eq!(q.types().len(), 1);
    }

    /// The overwhelmingly common case has to be provably what the line did
    /// before this module existed, not merely equivalent to it.
    #[test]
    fn a_line_with_no_syntax_in_it_is_its_own_term() {
        for line in ["11-D-0704", "report a1", "a.b.c", "(rev 2)"] {
            let q = Query::parse(line);
            assert_eq!(q.term(), line, "{line:?}");
            assert!(!q.is_filtered(), "{line:?}");
            assert_eq!(q.problem(), None, "{line:?}");
        }
    }

    // --- the type filter ----------------------------------------------------

    #[test]
    fn one_token_can_name_several_extensions() {
        let q = Query::parse("report ext:dwg,pdf");
        let got: Vec<&str> = q.types().iter().map(Ext::as_str).collect();
        assert_eq!(got, ["dwg", "pdf"]);
    }

    #[test]
    fn a_dot_before_the_extension_is_accepted_and_dropped() {
        assert_eq!(Query::parse("abc ext:.pdf"), Query::parse("abc ext:pdf"));
    }

    #[test]
    fn the_filter_keyword_is_recognised_whatever_its_case() {
        assert_eq!(Query::parse("abc EXT:PDF"), Query::parse("abc ext:pdf"));
    }

    /// `context:` and `next:pdf` are things somebody could reasonably be
    /// searching for, so the keyword only counts as a whole token.
    #[test]
    fn the_filter_keyword_is_recognised_only_as_a_whole_token() {
        for line in ["abc context:pdf", "abc next:pdf", "abc myext:pdf"] {
            let q = Query::parse(line);
            assert!(q.types().is_empty(), "{line:?} should not filter");
        }
    }

    /// Typed a character at a time, `ext:` exists for as long as it takes to
    /// reach the p. Reading it as "admit nothing" empties the results pane
    /// mid-word, which reads as the program having broken.
    #[test]
    fn an_ext_token_with_nothing_after_it_is_no_filter_at_all() {
        for line in ["report ext:", "report ext:,", "report ext:pdf,"] {
            let q = Query::parse(line);
            assert_eq!(q.term(), "report", "{line:?}");
            assert_eq!(q.problem(), None, "{line:?}");
        }
        assert!(Query::parse("report ext:").types().is_empty());
        assert_eq!(Query::parse("report ext:pdf,").types().len(), 1);
    }

    /// Pins the choice against prefix-matching the extension, which would make
    /// `ext:doc` quietly admit `.docx`.
    #[test]
    fn a_half_typed_extension_filters_on_exactly_what_was_typed() {
        let q = Query::parse("abc ext:pd");
        assert!(q.types().admits_name(b"x.pd"));
        assert!(!q.types().admits_name(b"x.pdf"));
    }

    #[test]
    fn an_extension_no_file_could_have_is_refused_with_a_reason() {
        for bad in ["", ".", "a*b", "toolongextension", "pd\u{e9}"] {
            assert_eq!(Ext::parse(bad), None, "{bad:?} should be refused");
        }
        assert_eq!(
            Query::parse("abc ext:a*b").problem(),
            Some(Problem::BadExtension)
        );
    }

    #[test]
    fn a_duplicate_extension_is_kept_only_once() {
        assert_eq!(Query::parse("abc ext:pdf,pdf,PDF").types().len(), 1);
    }

    // --- what counts as too short -------------------------------------------

    /// `ab ext:pdf` is a ten-character line and a two-character sweep across
    /// every name on the share. The minimum exists to bound the sweep, so it
    /// has to be counted on the thing that is swept for.
    #[test]
    fn the_minimum_length_applies_to_the_term_and_not_to_the_line() {
        assert_eq!(
            Query::parse("ab ext:pdf").check(),
            Err(QueryReject::TooShort {
                need: MIN_QUERY_LEN
            })
        );
        assert!(Query::parse("abc ext:pdf").check().is_ok());
    }

    /// "Every PDF on the share" is three hundred rows in directory order
    /// ranked by nothing, and an arena sweep with no needle to make it fast.
    #[test]
    fn a_line_that_is_nothing_but_a_filter_is_too_short_rather_than_everything() {
        assert_eq!(
            Query::parse("ext:pdf").check(),
            Err(QueryReject::TooShort {
                need: MIN_QUERY_LEN
            })
        );
    }

    #[test]
    fn a_lone_star_is_too_short_rather_than_a_match_for_everything() {
        assert_eq!(Query::parse("*").term(), "");
        assert!(!Query::parse("*").is_searchable());
    }

    // --- splitting a name ---------------------------------------------------

    #[test]
    fn the_last_dot_splits_the_stem_from_the_extension() {
        let name = b"11-D-0704.rev2.pdf";
        let (stem, ext) = split_stem(name);
        assert_eq!(&name[..stem], b"11-D-0704.rev2");
        assert_eq!(&name[ext..], b"pdf");
    }

    #[test]
    fn a_name_with_no_dot_has_no_extension_and_is_refused_by_any_filter() {
        let (stem, ext) = split_stem(b"README");
        assert_eq!(stem, 6);
        assert_eq!(ext, 6);
        assert!(!TypeFilter::new(&["pdf"]).admits_name(b"README"));
        assert!(TypeFilter::default().admits_name(b"README"));
    }

    /// A consequence of splitting on the last dot rather than a policy. Pinned
    /// so that it stays deliberate.
    #[test]
    fn a_leading_dot_leaves_an_empty_stem() {
        assert_eq!(split_stem(b".gitignore").0, 0);
    }

    // --- the sieve ----------------------------------------------------------

    #[test]
    fn a_prefix_search_takes_only_names_that_start_with_the_term() {
        let h = Hidden::none();
        let q = Query::parse("report*");
        let s = sieve(&h, b"report", &q);
        assert_eq!(s.admits(b"report_a.pdf", 0), Some(0));
        assert_eq!(s.admits(b"a_report.pdf", 2), None);
    }

    /// The headline test of the feature. `report_report.pdf` matches `report`
    /// at offset zero first, and the sweep records only that leftmost hit.
    /// Testing it against the end of the stem would drop a file whose stem
    /// does end in `report` - a false negative with no symptom on screen.
    #[test]
    fn a_suffix_search_finds_the_trailing_occurrence_not_the_leftmost_one() {
        let h = Hidden::none();
        let q = Query::parse("*report");
        let s = sieve(&h, b"report", &q);
        // The sweep hands us pos = 0, the leftmost occurrence.
        assert_eq!(s.admits(b"report_report.pdf", 0), Some(7));
        assert_eq!(s.admits(b"report_a.pdf", 0), None);
        assert_eq!(s.admits(b"a_report.pdf", 2), Some(2));
    }

    /// The dual: if any occurrence sits at zero then the leftmost one does,
    /// which is why a prefix costs a comparison and a suffix costs a memcmp.
    #[test]
    fn a_prefix_search_may_trust_the_leftmost_occurrence() {
        let h = Hidden::none();
        let q = Query::parse("ab*");
        let s = sieve(&h, b"ab", &q);
        assert_eq!(s.admits(b"abxab.pdf", 0), Some(0));
    }

    #[test]
    fn a_suffix_search_ignores_the_extension() {
        let h = Hidden::none();
        let q = Query::parse("*0704");
        let s = sieve(&h, b"0704", &q);
        assert_eq!(s.admits(b"11-d-0704.pdf", 5), Some(5));
        // No extension at all: the whole name is the stem.
        assert_eq!(s.admits(b"11-d-0704", 5), Some(5));
    }

    #[test]
    fn the_type_filter_and_the_hidden_list_both_still_apply() {
        let h = Hidden::new(&["bak"], false);
        let q = Query::parse("report ext:pdf");
        let s = sieve(&h, b"report", &q);
        assert_eq!(s.admits(b"a_report.pdf", 2), Some(2));
        assert_eq!(s.admits(b"a_report.dwg", 2), None, "wrong type");
        assert_eq!(s.admits(b"a_report.bak", 2), None, "hidden");
    }

    /// The file's own name did not match; the folder did. Testing it against
    /// the mode would empty the folder that is the whole reason the row is
    /// there.
    #[test]
    fn a_file_pulled_in_by_its_folder_is_not_judged_against_the_mode() {
        let h = Hidden::none();
        let q = Query::parse("0704* ext:pdf");
        let s = sieve(&h, b"0704", &q);
        assert!(s.admits_inherited(b"anything at all.pdf"));
        assert!(!s.admits_inherited(b"anything at all.dwg"));
    }

    #[test]
    fn a_folder_has_no_extension_so_its_whole_name_is_the_stem() {
        let h = Hidden::none();
        let q = Query::parse("*revisions");
        let s = sieve(&h, b"revisions", &q);
        assert_eq!(s.admits_folder(b"11.5 revisions", 5), Some(5));
    }

    // --- writing a line back out --------------------------------------------

    #[test]
    fn every_line_this_writes_parses_back_to_the_query_that_wrote_it() {
        for line in [
            "report",
            "report*",
            "*report",
            "report ext:pdf",
            "report* ext:dwg,pdf",
            "11-D-0704 survey ext:pdf",
        ] {
            let q = Query::parse(line);
            assert_eq!(Query::parse(&q.to_line()), q, "{line:?}");
        }
    }

    /// F3 has to come back to where it started, or the key is a trap.
    #[test]
    fn cycling_the_mode_three_times_returns_to_the_mode_it_started_in() {
        for m in MatchMode::ALL {
            assert_eq!(m.next().next().next(), m);
        }
    }

    #[test]
    fn cycling_the_mode_rewrites_the_line_and_keeps_the_filter() {
        let q = Query::parse("report ext:pdf");
        let next = q.with_mode(q.mode().next());
        assert_eq!(next.to_line(), "report* ext:pdf");
    }

    // --- what the empty pane will say ---------------------------------------

    #[test]
    fn an_unnarrowed_query_describes_nothing_rather_than_saying_so() {
        assert_eq!(Query::parse("report").describe(), None);
    }

    #[test]
    fn a_narrowed_query_says_what_was_left_out() {
        assert_eq!(
            Query::parse("report ext:pdf").describe().unwrap(),
            "Only .pdf files were looked at."
        );
        assert_eq!(
            Query::parse("report* ext:dwg,pdf").describe().unwrap(),
            "Only .dwg and .pdf files with it at the start of the name were looked at."
        );
        assert_eq!(
            Query::parse("*report").describe().unwrap(),
            "Only names with it at the end of the name were looked at."
        );
    }
}
