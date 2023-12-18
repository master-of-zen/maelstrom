use crate::parse_str;
use anyhow::{anyhow, Error, Result};
use combine::{
    attempt, between, choice, many, many1, optional, parser,
    parser::{
        char::{space, spaces, string},
        combinator::{lazy, no_partial},
    },
    satisfy, token, Parser, Stream,
};
use derive_more::From;
use globset::{Glob, GlobMatcher};
use regex::Regex;
use std::str::FromStr;

#[cfg(test)]
use regex_macro::regex;

#[derive(From, Debug, PartialEq, Eq)]
#[from(forward)]
pub struct MatcherParameter(pub String);

impl MatcherParameter {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        parser(|input| {
            let (open, committed) =
                choice((token('('), token('['), token('{'), token('<'), token('/')))
                    .parse_stream(input)
                    .into_result()?;
            let close = match open {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                '<' => '>',
                '/' => '/',
                _ => unreachable!(),
            };
            let mut count = 1;
            let mut contents = String::new();
            'outer: loop {
                let (chunk, _): (String, _) = many(satisfy(|c| c != open && c != close))
                    .parse_stream(input)
                    .into_result()?;
                contents += &chunk;

                while attempt(token(close)).parse_stream(input).is_ok() {
                    count -= 1;
                    if count == 0 {
                        break 'outer;
                    } else {
                        contents.push(close);
                    }
                }
                count += 1;
                token(open).parse_stream(input).into_result()?;
                contents.push(open);
            }

            Ok((contents, committed))
        })
        .map(Self)
    }
}

#[test]
fn matcher_parameter_test() {
    fn test_it(a: &str, b: &str) {
        assert_eq!(
            parse_str!(MatcherParameter, a),
            Ok(MatcherParameter(b.into()))
        );
    }
    test_it("[abc]", "abc");
    test_it("{abc}", "abc");
    test_it("<abc>", "abc");
    test_it("[(hello)]", "(hello)");
    test_it("((hello))", "(hello)");
    test_it("(([hello]))", "([hello])");
    test_it("(he[llo)", "he[llo");
    test_it("()", "");
    test_it("((()))", "(())");
    test_it("((a)(b))", "(a)(b)");

    fn test_err(a: &str) {
        assert!(matches!(parse_str!(MatcherParameter, a), Err(_)));
    }
    test_err("[1)");
    test_err("(((hello))");
}

pub fn err_construct<
    RetT,
    ErrorT: std::error::Error + Send + Sync + 'static,
    InputT: Stream<Token = char>,
>(
    mut inner: impl Parser<InputT, Output = String>,
    mut con: impl FnMut(&str) -> std::result::Result<RetT, ErrorT>,
) -> impl Parser<InputT, Output = RetT> {
    use combine::{
        error::{Commit, StreamError},
        ParseError,
    };
    parser(move |input: &mut InputT| {
        let position = input.position();
        let (s, committed) = inner.parse_stream(input).into_result()?;
        match con(&s) {
            Ok(r) => Ok((r, committed)),
            Err(e) => {
                let mut parse_error = InputT::Error::empty(position);
                parse_error.add(StreamError::other(e));
                Err(Commit::Commit(parse_error.into()))
            }
        }
    })
}

#[derive(Debug)]
pub struct GlobMatcherParameter(pub GlobMatcher);

impl PartialEq for GlobMatcherParameter {
    fn eq(&self, other: &Self) -> bool {
        self.0.glob() == other.0.glob()
    }
}

impl Eq for GlobMatcherParameter {}

impl GlobMatcherParameter {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        err_construct(MatcherParameter::parser().map(|v| v.0), Glob::new)
            .map(|g| Self(g.compile_matcher()))
    }
}

#[derive(Debug)]
pub struct RegexMatcherParameter(pub Regex);

impl From<&Regex> for RegexMatcherParameter {
    fn from(r: &Regex) -> Self {
        Self(r.clone())
    }
}

impl PartialEq for RegexMatcherParameter {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for RegexMatcherParameter {}

impl RegexMatcherParameter {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        err_construct(MatcherParameter::parser().map(|v| v.0), Regex::new).map(Self)
    }
}

#[test]
fn regex_parser_test() {
    parse_str!(RegexMatcherParameter, "/[a-z]/").unwrap();
    parse_str!(RegexMatcherParameter, "/*/").unwrap_err();
}

#[derive(Debug, PartialEq, Eq)]
pub enum Matcher {
    Equals(MatcherParameter),
    Contains(MatcherParameter),
    StartsWith(MatcherParameter),
    EndsWith(MatcherParameter),
    Matches(RegexMatcherParameter),
    Globs(GlobMatcherParameter),
}

impl Matcher {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        let arg = || MatcherParameter::parser();
        let regex = || RegexMatcherParameter::parser();
        let glob = || GlobMatcherParameter::parser();
        choice((
            attempt(string("equals").with(arg())).map(Self::Equals),
            attempt(string("contains").with(arg())).map(Self::Contains),
            attempt(string("starts_with").with(arg())).map(Self::StartsWith),
            attempt(string("ends_with").with(arg())).map(Self::EndsWith),
            attempt(string("matches").with(regex())).map(Self::Matches),
            string("globs").with(glob()).map(Self::Globs),
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompoundSelectorName {
    Name,
    Binary,
    Benchmark,
    Example,
    Test,
    Package,
}

impl CompoundSelectorName {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        choice((
            attempt(string("name")).map(|_| Self::Name),
            attempt(string("package")).map(|_| Self::Package),
            Self::parser_for_simple_selector(),
        ))
    }

    pub fn parser_for_simple_selector<InputT: Stream<Token = char>>(
    ) -> impl Parser<InputT, Output = Self> {
        choice((
            attempt(string("binary")).map(|_| Self::Binary),
            attempt(string("benchmark")).map(|_| Self::Benchmark),
            attempt(string("example")).map(|_| Self::Example),
            string("test").map(|_| Self::Test),
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CompoundSelector {
    pub name: CompoundSelectorName,
    pub matcher: Matcher,
}

impl CompoundSelector {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        (
            CompoundSelectorName::parser().skip(token('.')),
            Matcher::parser(),
        )
            .map(|(name, matcher)| Self { name, matcher })
    }
}

#[derive(Debug, PartialEq, Eq, From)]
pub enum SimpleSelectorName {
    All,
    Any,
    True,
    None,
    False,
    Library,
    #[from]
    Compound(CompoundSelectorName),
}

impl SimpleSelectorName {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        choice((
            attempt(string("all")).map(|_| Self::All),
            attempt(string("any")).map(|_| Self::Any),
            attempt(string("true")).map(|_| Self::True),
            attempt(string("none")).map(|_| Self::None),
            attempt(string("false")).map(|_| Self::False),
            attempt(string("library")).map(|_| Self::Library),
            CompoundSelectorName::parser_for_simple_selector().map(Self::Compound),
        ))
    }
}

#[derive(Debug, PartialEq, Eq, From)]
#[from(types(CompoundSelectorName))]
pub struct SimpleSelector {
    pub name: SimpleSelectorName,
}

impl SimpleSelector {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        SimpleSelectorName::parser()
            .skip(optional(string("()")))
            .map(|name| Self { name })
    }
}

#[derive(Debug, PartialEq, Eq, From)]
pub enum SimpleExpression {
    #[from(types(OrExpression))]
    Or(Box<OrExpression>),
    #[from(types(SimpleSelectorName, CompoundSelectorName))]
    SimpleSelector(SimpleSelector),
    #[from]
    CompoundSelector(CompoundSelector),
}

impl From<AndExpression> for SimpleExpression {
    fn from(a: AndExpression) -> Self {
        OrExpression::from(a).into()
    }
}

impl SimpleExpression {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        let or_parser = || no_partial(lazy(|| OrExpression::parser())).boxed();
        choice((
            attempt(between(
                token('(').skip(spaces()),
                spaces().with(token(')')),
                or_parser(),
            ))
            .map(|o| Self::Or(Box::new(o))),
            attempt(CompoundSelector::parser().map(Self::CompoundSelector)),
            attempt(SimpleSelector::parser().map(Self::SimpleSelector)),
        ))
    }
}

fn not_operator<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = &'static str> {
    choice((string("!"), string("~"), string("not").skip(spaces1())))
}

#[derive(Debug, PartialEq, Eq, From)]
pub enum NotExpression {
    Not(Box<NotExpression>),
    #[from(types(SimpleSelector, SimpleSelectorName, CompoundSelector, OrExpression))]
    Simple(SimpleExpression),
}

impl NotExpression {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        let self_parser = || no_partial(lazy(|| Self::parser())).boxed();
        choice((
            attempt(not_operator().with(self_parser().map(|e| Self::Not(Box::new(e))))),
            SimpleExpression::parser().map(Self::Simple),
        ))
    }
}

fn spaces1<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = String> {
    many1(space())
}

fn and_operator<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = &'static str> {
    attempt(between(
        spaces(),
        spaces(),
        choice((attempt(string("&&")), string("&"), string("+"))),
    ))
    .or(spaces1().with(string("and")).skip(spaces1()))
}

fn diff_operator<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = &'static str> {
    attempt(between(
        spaces(),
        spaces(),
        choice((string("\\"), string("-"))),
    ))
    .or(spaces1().with(string("minus")).skip(spaces1()))
}

#[derive(Debug, PartialEq, Eq, From)]
pub enum AndExpression {
    And(NotExpression, Box<AndExpression>),
    Diff(NotExpression, Box<AndExpression>),
    #[from(types(SimpleExpression, SimpleSelector, SimpleSelectorName, CompoundSelector))]
    Not(NotExpression),
}

impl AndExpression {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        let self_parser = || no_partial(lazy(|| Self::parser())).boxed();
        choice((
            attempt((NotExpression::parser(), and_operator(), self_parser()))
                .map(|(n, _, a)| Self::And(n, Box::new(a))),
            attempt((NotExpression::parser(), diff_operator(), self_parser()))
                .map(|(n, _, a)| Self::Diff(n, Box::new(a))),
            NotExpression::parser().map(Self::Not),
        ))
    }
}

fn or_operator<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = &'static str> {
    attempt(between(
        spaces(),
        spaces(),
        choice((attempt(string("||")), string("|"))),
    ))
    .or(spaces1().with(string("or")).skip(spaces1()))
}

#[derive(Debug, PartialEq, Eq, From)]
pub enum OrExpression {
    Or(AndExpression, Box<OrExpression>),
    #[from(types(
        NotExpression,
        SimpleExpression,
        SimpleSelector,
        SimpleSelectorName,
        CompoundSelector
    ))]
    And(AndExpression),
}

impl OrExpression {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        let self_parser = || no_partial(lazy(|| Self::parser())).boxed();
        choice((
            attempt((AndExpression::parser(), or_operator(), self_parser()))
                .map(|(a, _, o)| Self::Or(a, Box::new(o))),
            AndExpression::parser().map(Self::And),
        ))
    }
}

#[derive(Debug, PartialEq, Eq, From)]
#[from(types(NotExpression, AndExpression))]
pub struct Pattern(pub OrExpression);

impl Pattern {
    pub fn parser<InputT: Stream<Token = char>>() -> impl Parser<InputT, Output = Self> {
        OrExpression::parser().map(Self)
    }
}

impl FromStr for Pattern {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        parse_str!(Self, s).map_err(|e| anyhow!("Failed to parse pattern: {e}"))
    }
}

#[macro_export]
macro_rules! parse_str {
    ($ty:ty, $input:expr) => {{
        use combine::{EasyParser as _, Parser as _};
        <$ty>::parser()
            .skip(combine::eof())
            .easy_parse(combine::stream::position::Stream::new($input))
            .map(|x| x.0)
    }};
}

#[test]
fn simple_expr() {
    use CompoundSelectorName::*;
    use SimpleSelectorName::*;

    fn test_it(a: &str, s: impl Into<SimpleExpression>) {
        assert_eq!(parse_str!(SimpleExpression, a), Ok(s.into()));
    }
    test_it("all", All);
    test_it("all()", All);
    test_it("any", Any);
    test_it("any()", Any);
    test_it("true", True);
    test_it("true()", True);
    test_it("none", None);
    test_it("none()", None);
    test_it("false", False);
    test_it("false()", False);
    test_it("library", Library);
    test_it("library()", Library);

    test_it("binary", Binary);
    test_it("binary()", Binary);
    test_it("benchmark", Benchmark);
    test_it("benchmark()", Benchmark);
    test_it("example", Example);
    test_it("example()", Example);
    test_it("test", Test);
    test_it("test()", Test);

    fn test_it_err(a: &str) {
        assert!(parse_str!(SimpleExpression, a).is_err());
    }
    test_it_err("name");
    test_it_err("name()");
    test_it_err("package");
    test_it_err("package()");
}

#[test]
fn simple_expr_compound() {
    use CompoundSelectorName::*;
    use Matcher::*;

    fn test_it(a: &str, name: CompoundSelectorName, matcher: Matcher) {
        assert_eq!(
            parse_str!(SimpleExpression, a),
            Ok(CompoundSelector { name, matcher }.into())
        );
    }
    test_it("name.matches<foo>", Name, Matches(regex!("foo").into()));
    test_it("test.equals([a-z].*)", Test, Equals("[a-z].*".into()));
    test_it(
        "binary.starts_with<(hi)>",
        Binary,
        StartsWith("(hi)".into()),
    );
    test_it(
        "benchmark.ends_with[hey?]",
        Benchmark,
        EndsWith("hey?".into()),
    );
    test_it(
        "example.contains{s(oi)l}",
        Example,
        Contains("s(oi)l".into()),
    );
}

#[test]
fn pattern_simple_boolean_expr() {
    fn test_it(a: &str, pattern: impl Into<Pattern>) {
        assert_eq!(parse_str!(Pattern, a), Ok(pattern.into()));
    }
    test_it(
        "!all",
        NotExpression::Not(Box::new(SimpleSelectorName::All.into())),
    );
    test_it(
        "all && any",
        AndExpression::And(
            SimpleSelectorName::All.into(),
            Box::new(SimpleSelectorName::Any.into()),
        ),
    );
    test_it(
        "all || any",
        OrExpression::Or(
            SimpleSelectorName::All.into(),
            Box::new(SimpleSelectorName::Any.into()),
        ),
    );
}

#[test]
fn pattern_longer_boolean_expr() {
    fn test_it(a: &str, pattern: impl Into<Pattern>) {
        assert_eq!(parse_str!(Pattern, a), Ok(pattern.into()));
    }
    test_it(
        "all || any || none",
        OrExpression::Or(
            SimpleSelectorName::All.into(),
            Box::new(
                OrExpression::Or(
                    SimpleSelectorName::Any.into(),
                    Box::new(SimpleSelectorName::None.into()),
                )
                .into(),
            ),
        ),
    );
    test_it(
        "all || any && none",
        OrExpression::Or(
            SimpleSelectorName::All.into(),
            Box::new(
                AndExpression::And(
                    SimpleSelectorName::Any.into(),
                    Box::new(SimpleSelectorName::None.into()),
                )
                .into(),
            ),
        ),
    );
    test_it(
        "all && any || none",
        OrExpression::Or(
            AndExpression::And(
                SimpleSelectorName::All.into(),
                Box::new(SimpleSelectorName::Any.into()),
            ),
            Box::new(SimpleSelectorName::None.into()),
        ),
    );
}

#[test]
fn pattern_complicated_boolean_expr() {
    fn test_it(a: &str, pattern: impl Into<Pattern>) {
        assert_eq!(parse_str!(Pattern, a), Ok(pattern.into()));
    }
    test_it(
        "( all || any ) && none - library",
        AndExpression::And(
            OrExpression::Or(
                SimpleSelectorName::All.into(),
                Box::new(SimpleSelectorName::Any.into()),
            )
            .into(),
            Box::new(AndExpression::Diff(
                SimpleSelectorName::None.into(),
                Box::new(SimpleSelectorName::Library.into()),
            )),
        ),
    );
    test_it(
        "!( all || any ) && none",
        AndExpression::And(
            NotExpression::Not(Box::new(
                OrExpression::Or(
                    SimpleSelectorName::All.into(),
                    Box::new(SimpleSelectorName::Any.into()),
                )
                .into(),
            )),
            Box::new(SimpleSelectorName::None.into()),
        ),
    );

    test_it(
        "not ( all or any ) and none minus library",
        AndExpression::And(
            NotExpression::Not(Box::new(
                OrExpression::Or(
                    SimpleSelectorName::All.into(),
                    Box::new(SimpleSelectorName::Any.into()),
                )
                .into(),
            )),
            Box::new(AndExpression::Diff(
                SimpleSelectorName::None.into(),
                Box::new(SimpleSelectorName::Library.into()),
            )),
        ),
    );
}

#[test]
fn pattern_complicated_boolean_expr_compound() {
    fn test_it(a: &str, pattern: impl Into<Pattern>) {
        assert_eq!(parse_str!(Pattern, a), Ok(pattern.into()));
    }

    test_it(
        "binary.starts_with(hi) && name.matches/([a-z]+::)*[a-z]+/",
        AndExpression::And(
            CompoundSelector {
                name: CompoundSelectorName::Binary,
                matcher: Matcher::StartsWith("hi".into()),
            }
            .into(),
            Box::new(
                CompoundSelector {
                    name: CompoundSelectorName::Name,
                    matcher: Matcher::Matches(regex!("([a-z]+::)*[a-z]+").into()),
                }
                .into(),
            ),
        ),
    );

    test_it(
        "( binary.starts_with(hi) && name.matches/([a-z]+::)*[a-z]+/ ) || benchmark.ends_with(jo)",
        OrExpression::Or(
            NotExpression::Simple(
                AndExpression::And(
                    CompoundSelector {
                        name: CompoundSelectorName::Binary,
                        matcher: Matcher::StartsWith("hi".into()),
                    }
                    .into(),
                    Box::new(
                        CompoundSelector {
                            name: CompoundSelectorName::Name,
                            matcher: Matcher::Matches(regex!("([a-z]+::)*[a-z]+").into()),
                        }
                        .into(),
                    ),
                )
                .into(),
            )
            .into(),
            Box::new(
                CompoundSelector {
                    name: CompoundSelectorName::Benchmark,
                    matcher: Matcher::EndsWith("jo".into()),
                }
                .into(),
            ),
        ),
    );
}
