//! Parses one search line into a term, a match mode and a type filter.
//!
//! # Syntax lives in the line
//!
//! Mode and type are written into the search line rather than held as
//! toggles, so they are visible, recalled with history and copied with the
//! line. F3 and F4 edit the line, so what runs is always what is shown.
//!
//! # Only characters Windows forbids in a name
//!
//! Windows refuses `" * ? < > | : \ /` in a filename, so syntax built from
//! them cannot collide with a search term. Only `*` and `:` are used. `^` and
//! `$` are legal in names and so cannot be anchors. Quotes are avoided because
//! a paired delimiter leaves the line invalid while it is being typed; every
//! token here is a valid query at every keystroke.
//!
//! # A bare `.pdf` is not a type filter
//!
//! `.pdf` is a legal substring of a name (`report .pdf.bak`), so it stays part
//! of the term. `ext:` cannot occur in a name.
//!
//! # The term is never split on whitespace
//!
//! Filenames on these shares contain spaces (`AB-C-0704 survey.pdf`), so the
//! term is the line minus the byte ranges of the filter tokens, never a list
//! of words.
//!
//! # Parsing cannot fail
//!
//! The line is re-parsed on every keystroke and passes through incomplete
//! states such as a bare `ext:`. [`Query::parse`] is total, and anything it
//! cannot honour is attached to the result as a [`Problem`].

use crate::config::MIN_TERM_LEN;
use crate::config::hidden::Hidden;
use crate::util::fold;

/// Where the term has to sit within the name.
///
/// Anchored forms apply to the stem (the name without its final extension),
/// since [`TypeFilter`] handles extensions and almost every name ends in one.
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
/// A fixed bound lets an extension sit inline with no allocation on the
/// match path.
pub const MAX_EXT_LEN: usize = 8;

/// One extension a query admits: folded, and without its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext {
    bytes: [u8; MAX_EXT_LEN],
    len: u8,
}

impl Ext {
    /// Returns `None` for anything no file can have as an extension.
    ///
    /// One leading dot is accepted and dropped, so `ext:.pdf` equals `ext:pdf`.
    ///
    /// Non-ASCII is refused for the reason [`Hidden::new`] refuses it: the arena
    /// fold declines folds that change byte length, so a non-ASCII extension
    /// would compare differently against the arena and the original name.
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
/// Empty means every extension, so a bare `ext:` while typing means "no
/// filter" rather than emptying the results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeFilter {
    /// `None` for no filter, behind a thin pointer.
    ///
    /// Every `AppEvent` carries a [`Query`], so inline storage here costs every
    /// event in the shared channel. The common unfiltered case costs eight
    /// bytes. `Box<[_]>` would be a 16-byte fat pointer, enough to make
    /// `VerifyMsg` worth boxing.
    // The thin pointer is intentional; Clippy flags this shape as accidental
    // double indirection.
    #[allow(clippy::box_collection)]
    exts: Option<Box<Vec<Ext>>>,
}

impl TypeFilter {
    /// Builds a filter from extension names, dropping the ones no file could
    /// have.
    ///
    /// Duplicates are removed, since each costs a comparison per candidate.
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

    /// Returns whether a folded name whose extension starts at `ext_start` is
    /// one of these types.
    ///
    /// For a dotless name `ext_start` is the name's length, giving an empty
    /// extension; no [`Ext`] is empty, so a non-empty filter refuses it.
    #[inline]
    pub fn admits(&self, name: &[u8], ext_start: usize) -> bool {
        let Some(exts) = &self.exts else {
            return true;
        };
        let ext = &name[ext_start..];
        exts.iter().any(|e| e.as_bytes() == ext)
    }

    /// The same check for a name whose stem has not been located yet.
    #[inline]
    pub fn admits_name(&self, name: &[u8]) -> bool {
        if self.exts.is_none() {
            return true;
        }
        self.admits(name, split_stem(name).1)
    }
}

/// Returns the end of the stem, where the extension starts.
///
/// The last dot splits: `AB-C-0704.rev2.pdf` has the stem `AB-C-0704.rev2`.
/// `README` has an empty extension and is refused by every type filter.
/// `.gitignore` has an empty stem and so never matches a suffix; a test pins
/// this.
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
    /// Refused rather than read literally: `*` cannot occur in a Windows
    /// filename, so a literal reading would silently return nothing.
    InteriorStar,
    /// `ext:` naming something no file can have as an extension.
    BadExtension,
}

impl Problem {
    pub fn detail(self) -> &'static str {
        match self {
            Self::InteriorStar => "* is only allowed at the start or end",
            Self::BadExtension => "not a valid file extension",
        }
    }
}

/// Why a query was not run at all.
///
/// Every variant is a property of the input rather than of an index, so the
/// check is made once per search rather than per share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryReject {
    /// There is no term at all.
    ///
    /// Counted on the term, not the line: `ext:pdf` has nothing to search for.
    Empty,
    /// Contains a NUL byte.
    ///
    /// Rejected rather than stripped, which would let `a\0bc` match `abc`. This
    /// also guarantees a match can never span two arena entries.
    ContainsNul,
    /// The line holds syntax that could not be honoured.
    Syntax(Problem),
}

impl QueryReject {
    pub fn detail(&self) -> String {
        match self {
            Self::Empty => "nothing to search for".into(),
            Self::ContainsNul => "contains a character that cannot be searched".into(),
            Self::Syntax(p) => p.detail().into(),
        }
    }
}

/// The token that introduces a type filter.
const EXT_PREFIX: &str = "ext:";

/// The types F4 cycles through, in order.
///
/// PDF documents and DWG (CAD) files are the types the panel is tuned for.
/// Any other extension is reachable by typing `ext:`.
pub const CYCLE_EXTENSIONS: &[&str] = &["pdf", "dwg"];

/// One search line, parsed.
///
/// The parser only recognises syntax spelled with characters Windows forbids
/// in a name and passes everything else through, so the term is literal.
/// [`crate::search::pattern`] relies on this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// `Box<str>` rather than `String`: a parsed query is never appended to, and
    /// the spare capacity field would cost eight bytes on every `AppEvent`.
    term: Box<str>,
    mode: MatchMode,
    types: TypeFilter,
    problem: Option<Problem>,
}

impl Query {
    /// A plain substring search.
    pub fn contains(term: impl AsRef<str>) -> Self {
        Self {
            term: term.as_ref().into(),
            ..Self::default()
        }
    }

    /// Parses a search line. Total: see the module docs.
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
            // Skipping empty pieces makes `ext:`, `ext:,` and `ext:pdf,` mean the
            // same as the filter being typed.
            for piece in value.split(',').filter(|p| !p.trim().is_empty()) {
                match Ext::parse(piece) {
                    Some(e) if !exts.contains(&e) => exts.push(e),
                    Some(_) => {}
                    None => problem = Some(Problem::BadExtension),
                }
            }
        }

        // Anchors are stripped one at a time so `*report*` equals `report`.
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

    /// Returns whether anything beyond a plain substring search was asked for.
    ///
    /// The sweep branches on this once so an unfiltered search takes the plain
    /// path.
    #[inline]
    pub fn is_filtered(&self) -> bool {
        self.mode != MatchMode::Contains || !self.types.is_empty()
    }

    /// Judges the query without consulting any index.
    pub fn check(&self) -> Result<(), QueryReject> {
        if let Some(p) = self.problem {
            return Err(QueryReject::Syntax(p));
        }
        if self.term.chars().count() < MIN_TERM_LEN {
            return Err(QueryReject::Empty);
        }
        if fold::fold_query(&self.term).contains(&0) {
            return Err(QueryReject::ContainsNul);
        }
        Ok(())
    }

    /// Returns whether this is worth dispatching at all.
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
    /// A filter F4 could not have produced (typed by hand, or several
    /// extensions) cycles back to no filter.
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
    /// Round-trips through [`Self::parse`], so F3 and F4 can edit the line and
    /// the line stays the only place the query lives.
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
    /// `None` when nothing was narrowed, so the caller adds no line.
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

/// Returns the rest of the token after a leading `ext:` in any case, or
/// `None` for ordinary text.
///
/// Matched as a whole token, so `context:` and `next:pdf` stay in the term.
fn strip_ext_prefix(token: &str) -> Option<&str> {
    let head = token.get(..EXT_PREFIX.len())?;
    head.eq_ignore_ascii_case(EXT_PREFIX)
        .then(|| &token[EXT_PREFIX.len()..])
}

/// `.pdf and .tif`, for a sentence.
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

/// Returns whether a folded name satisfies a query's mode and type filter,
/// and if so which occurrence of the needle justified it.
///
/// The sweep, [`crate::search::verify`]'s audit oracle and
/// [`crate::search::pattern::confirms`] must all use this one function: a
/// second implementation would let the audit disagree with the index and
/// switch server-side filtering off.
///
/// `pos` is the leftmost occurrence of `needle` in `name`. See
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
/// Combines the query and hidden-file rules so the sweep asks one question
/// per candidate, answered in one place.
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

    /// Returns whether `name` is admitted, and if so which occurrence of the
    /// needle justified it.
    ///
    /// `name` is one entry's folded bytes without its separator, and `pos` is
    /// the leftmost occurrence of the needle in it, which the sweep already
    /// holds.
    ///
    /// # Why the leftmost occurrence is not the one tested
    ///
    /// The sweep records only the leftmost occurrence per entry. That is correct
    /// for a prefix, but not for a suffix: `report_report.pdf` matches `report`
    /// at offset zero first, and testing that against the stem's end would drop
    /// a valid match. The suffix form therefore re-checks the stem's tail.
    #[inline]
    pub fn admits(&self, name: &[u8], pos: u32) -> Option<u32> {
        if self.hidden.hides_folded(name) {
            return None;
        }
        admits(name, self.needle, pos, self.mode, self.types)
    }

    /// The same check on the slow, fully Unicode-correct path.
    ///
    /// That path lowercases each name into a separate string, and
    /// `to_lowercase` does not preserve byte length, so the stem and needle are
    /// located again in `lower`.
    ///
    /// `name` is the original, for [`Hidden::hides`], which folds ASCII case
    /// itself. The type filter is compared against `lower`, so a non-ASCII
    /// extension is not matched, consistent with [`Ext::parse`].
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

    /// Returns whether a file included because its folder matched is admitted.
    ///
    /// The mode is not applied, since the file's own name did not match; only
    /// the type and hidden filters apply.
    #[inline]
    pub fn admits_inherited(&self, name: &[u8]) -> bool {
        !self.hidden.hides_folded(name) && self.types.admits_name(name)
    }

    /// Returns whether a folder name satisfies the mode.
    ///
    /// A folder has no extension, so the type filter does not apply and the
    /// whole name is the stem.
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

    /// `*` cannot occur in a Windows filename, so a literal reading would
    /// silently return nothing.
    #[test]
    fn a_star_in_the_middle_is_refused_rather_than_searched_for_literally() {
        let q = Query::parse("re*port");
        assert_eq!(q.problem(), Some(Problem::InteriorStar));
        assert_eq!(q.check(), Err(QueryReject::Syntax(Problem::InteriorStar)));
    }

    /// Filenames contain spaces, so the line is not split into words.
    #[test]
    fn a_term_with_spaces_in_it_survives_the_filter_being_lifted_out() {
        let q = Query::parse("AB-C-0704 survey ext:pdf");
        assert_eq!(q.term(), "AB-C-0704 survey");
        assert_eq!(q.types().len(), 1);
    }

    /// The plain case parses to exactly a plain substring search.
    #[test]
    fn a_line_with_no_syntax_in_it_is_its_own_term() {
        for line in ["AB-C-0704", "report a1", "a.b.c", "(rev 2)"] {
            let q = Query::parse(line);
            assert_eq!(q.term(), line, "{line:?}");
            assert!(!q.is_filtered(), "{line:?}");
            assert_eq!(q.problem(), None, "{line:?}");
        }
    }

    // --- the type filter ----------------------------------------------------

    #[test]
    fn one_token_can_name_several_extensions() {
        let q = Query::parse("report ext:pdf,tif");
        let got: Vec<&str> = q.types().iter().map(Ext::as_str).collect();
        assert_eq!(got, ["pdf", "tif"]);
    }

    #[test]
    fn a_dot_before_the_extension_is_accepted_and_dropped() {
        assert_eq!(Query::parse("abc ext:.pdf"), Query::parse("abc ext:pdf"));
    }

    #[test]
    fn the_filter_keyword_is_recognised_whatever_its_case() {
        assert_eq!(Query::parse("abc EXT:PDF"), Query::parse("abc ext:pdf"));
    }

    /// The keyword only counts as a whole token.
    #[test]
    fn the_filter_keyword_is_recognised_only_as_a_whole_token() {
        for line in ["abc context:pdf", "abc next:pdf", "abc myext:pdf"] {
            let q = Query::parse(line);
            assert!(q.types().is_empty(), "{line:?} should not filter");
        }
    }

    /// A bare `ext:` while typing does not empty the results.
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

    /// Extensions match exactly, so `ext:doc` does not admit `.docx`.
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

    // --- what counts as nothing ---------------------------------------------

    /// The floor is counted on the term, not on the line.
    #[test]
    fn the_minimum_length_applies_to_the_term_and_not_to_the_line() {
        assert_eq!(Query::parse("  ext:pdf").check(), Err(QueryReject::Empty));
        assert!(
            Query::parse("a ext:pdf").check().is_ok(),
            "one character is a search now"
        );
    }

    /// A filter with no term is not a search.
    #[test]
    fn a_line_that_is_nothing_but_a_filter_is_empty_rather_than_everything() {
        assert_eq!(Query::parse("ext:pdf").check(), Err(QueryReject::Empty));
    }

    /// A single character is a search.
    #[test]
    fn one_character_is_a_search() {
        assert!(Query::parse("a").check().is_ok());
        assert!(Query::parse("1").check().is_ok());
    }

    #[test]
    fn a_lone_star_is_too_short_rather_than_a_match_for_everything() {
        assert_eq!(Query::parse("*").term(), "");
        assert!(!Query::parse("*").is_searchable());
    }

    // --- splitting a name ---------------------------------------------------

    #[test]
    fn the_last_dot_splits_the_stem_from_the_extension() {
        let name = b"AB-C-0704.rev2.pdf";
        let (stem, ext) = split_stem(name);
        assert_eq!(&name[..stem], b"AB-C-0704.rev2");
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

    /// A consequence of splitting on the last dot, pinned by this test.
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

    /// `report_report.pdf` matches `report` at offset zero first, and the sweep
    /// records only that hit; the suffix check must still admit it.
    #[test]
    fn a_suffix_search_finds_the_trailing_occurrence_not_the_leftmost_one() {
        let h = Hidden::none();
        let q = Query::parse("*report");
        let s = sieve(&h, b"report", &q);
        // The sweep passes pos = 0, the leftmost occurrence.
        assert_eq!(s.admits(b"report_report.pdf", 0), Some(7));
        assert_eq!(s.admits(b"report_a.pdf", 0), None);
        assert_eq!(s.admits(b"a_report.pdf", 2), Some(2));
    }

    /// If any occurrence sits at zero then the leftmost one does, so a prefix
    /// needs only a comparison.
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
        assert_eq!(s.admits(b"ab-c-0704.pdf", 5), Some(5));
        // No extension at all: the whole name is the stem.
        assert_eq!(s.admits(b"ab-c-0704", 5), Some(5));
    }

    #[test]
    fn the_type_filter_and_the_hidden_list_both_still_apply() {
        let h = Hidden::new(&["bak"], false);
        let q = Query::parse("report ext:pdf");
        let s = sieve(&h, b"report", &q);
        assert_eq!(s.admits(b"a_report.pdf", 2), Some(2));
        assert_eq!(s.admits(b"a_report.tif", 2), None, "wrong type");
        assert_eq!(s.admits(b"a_report.bak", 2), None, "hidden");
    }

    /// The file's own name did not match; the folder did. The mode is not
    /// applied to it.
    #[test]
    fn a_file_pulled_in_by_its_folder_is_not_judged_against_the_mode() {
        let h = Hidden::none();
        let q = Query::parse("0704* ext:pdf");
        let s = sieve(&h, b"0704", &q);
        assert!(s.admits_inherited(b"anything at all.pdf"));
        assert!(!s.admits_inherited(b"anything at all.tif"));
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
            "report* ext:pdf,tif",
            "AB-C-0704 survey ext:pdf",
        ] {
            let q = Query::parse(line);
            assert_eq!(Query::parse(&q.to_line()), q, "{line:?}");
        }
    }

    /// Cycling F3 returns to the starting mode.
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
            Query::parse("report* ext:pdf,tif").describe().unwrap(),
            "Only .pdf and .tif files with it at the start of the name were looked at."
        );
        assert_eq!(
            Query::parse("*report").describe().unwrap(),
            "Only names with it at the end of the name were looked at."
        );
    }
}
