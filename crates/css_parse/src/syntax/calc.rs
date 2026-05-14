use crate::{
	AssociatedWhitespaceRules, Cursor, CursorSink, Diagnostic, FunctionBlock, Kind, KindSet, Parse, Parser, Peek,
	Result as ParserResult, SemanticEq, Span, T, ToCursors, ToSpan,
};
use bumpalo::collections::Vec;

/// Names of math functions defined in
/// <https://www.w3.org/TR/css-values-4/#math> that this AST recognizes.
///
/// `calc()` takes a single `<calc-sum>`. The others accept comma-separated
/// `<calc-sum>` arguments with function-specific semantics that we don't
/// enforce at this layer — we just preserve their tokens losslessly.
fn is_math_function(name: &str) -> bool {
	matches!(
		name.trim_end_matches('('),
		"calc" | "min" | "max" | "clamp" | "mod" | "rem"
	)
}

/// A `calc()`/`min()`/`max()`/`clamp()`/`mod()`/`rem()` expression.
///
/// The body is parsed into a [CalcSum] tree so the parser can attach explicit
/// `AssociatedWhitespaceRules` to the `+`/`-` operators (which the CSS spec
/// requires to be surrounded by whitespace). `*`/`/` operators are left
/// untouched because the spec does not require whitespace around them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcExpression<'a> {
	pub name: T![Function],
	pub args: Vec<'a, CalcArg<'a>>,
	pub close: T![')'],
}

/// A single comma-delimited argument inside a math function.
///
/// `calc()` always has exactly one `CalcArg` with no trailing comma; the other
/// math functions can have multiple, separated by commas.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcArg<'a> {
	pub sum: CalcSum<'a>,
	pub trailing_comma: Option<T![,]>,
}

/// `<calc-sum> = <calc-product> [ ['+' | '-'] <calc-product> ]*`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcSum<'a> {
	pub head: CalcProduct<'a>,
	pub tail: Vec<'a, (CalcSumOp, CalcProduct<'a>)>,
}

/// A `+` or `-` operator inside a `<calc-sum>`.
///
/// The wrapped delim always carries
/// `AssociatedWhitespaceRules::EnforceBefore | EnforceAfter`, so serialization
/// emits the spec-required whitespace even when the source omitted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcSumOp(pub T![Delim]);

/// `<calc-product> = <calc-value> [ ['*' | '/'] <calc-value> ]*`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcProduct<'a> {
	pub head: CalcValue<'a>,
	pub tail: Vec<'a, (CalcProductOp, CalcValue<'a>)>,
}

/// A `*` or `/` operator inside a `<calc-product>`. The wrapped delim keeps
/// whatever `AssociatedWhitespaceRules` the lexer gave it; the spec does not
/// require whitespace around `*`/`/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcProductOp(pub T![Delim]);

/// A leaf value inside a `<calc-product>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(untagged))]
pub enum CalcValue<'a> {
	Number(T![Number]),
	Dimension(T![Dimension]),
	Ident(T![Ident]),
	/// A nested `calc()` / `min()` / `max()` / etc.
	Calc(Box<CalcExpression<'a>>),
	/// Any other function call within a calc expression (e.g. `var(--x)`,
	/// `env(safe-area-inset-top)`). Parsed losslessly as a generic
	/// [FunctionBlock].
	Function(FunctionBlock<'a>),
	/// A parenthesized sub-expression: `(<calc-sum>)`.
	Parens(CalcParens<'a>),
}

/// A parenthesized sub-expression inside a calc body.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde())]
pub struct CalcParens<'a> {
	pub open: T!['('],
	pub sum: Box<CalcSum<'a>>,
	pub close: T![')'],
}

impl<'a> Peek<'a> for CalcExpression<'a> {
	const PEEK_KINDSET: KindSet = KindSet::new(&[Kind::Function]);

	fn peek<I>(p: &Parser<'a, I>, c: Cursor) -> bool
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		c == Kind::Function && is_math_function(c.str_slice(p.source_text))
	}
}

impl<'a> Parse<'a> for CalcExpression<'a> {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let c = p.peek_n(1);
		if !Self::peek(p, c) {
			Err(Diagnostic::new(c, Diagnostic::unexpected))?;
		}
		let name = p.parse::<T![Function]>()?;
		let mut args = Vec::new_in(p.bump());
		loop {
			let sum = p.parse::<CalcSum>()?;
			let trailing_comma = if <T![,]>::peek(p, p.peek_n(1)) { Some(p.parse::<T![,]>()?) } else { None };
			let has_comma = trailing_comma.is_some();
			args.push(CalcArg { sum, trailing_comma });
			if !has_comma {
				break;
			}
		}
		let close = p.parse::<T![')']>()?;
		Ok(Self { name, args, close })
	}
}

impl<'a> ToCursors for CalcExpression<'a> {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.name, s);
		for arg in &self.args {
			ToCursors::to_cursors(&arg.sum, s);
			if let Some(comma) = arg.trailing_comma {
				ToCursors::to_cursors(&comma, s);
			}
		}
		ToCursors::to_cursors(&self.close, s);
	}
}

impl<'a> ToSpan for CalcExpression<'a> {
	fn to_span(&self) -> Span {
		self.name.to_span() + self.close.to_span()
	}
}

impl<'a> SemanticEq for CalcExpression<'a> {
	fn semantic_eq(&self, other: &Self) -> bool {
		if !self.name.semantic_eq(&other.name) || self.args.len() != other.args.len() {
			return false;
		}
		self.args.iter().zip(other.args.iter()).all(|(a, b)| a.sum.semantic_eq(&b.sum))
	}
}

impl<'a> Parse<'a> for CalcSum<'a> {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let head = p.parse::<CalcProduct>()?;
		let mut tail = Vec::new_in(p.bump());
		while <CalcSumOp>::peek(p, p.peek_n(1)) {
			let op = p.parse::<CalcSumOp>()?;
			let rhs = p.parse::<CalcProduct>()?;
			tail.push((op, rhs));
		}
		Ok(Self { head, tail })
	}
}

impl<'a> ToCursors for CalcSum<'a> {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.head, s);
		for (op, rhs) in &self.tail {
			ToCursors::to_cursors(op, s);
			ToCursors::to_cursors(rhs, s);
		}
	}
}

impl<'a> ToSpan for CalcSum<'a> {
	fn to_span(&self) -> Span {
		match self.tail.last() {
			Some((_, last)) => self.head.to_span() + last.to_span(),
			None => self.head.to_span(),
		}
	}
}

impl<'a> SemanticEq for CalcSum<'a> {
	fn semantic_eq(&self, other: &Self) -> bool {
		if !self.head.semantic_eq(&other.head) || self.tail.len() != other.tail.len() {
			return false;
		}
		self.tail
			.iter()
			.zip(other.tail.iter())
			.all(|((op_a, rhs_a), (op_b, rhs_b))| op_a.0.semantic_eq(&op_b.0) && rhs_a.semantic_eq(rhs_b))
	}
}

impl<'a> Peek<'a> for CalcSumOp {
	const PEEK_KINDSET: KindSet = KindSet::new(&[Kind::Delim]);

	fn peek<I>(_p: &Parser<'a, I>, c: Cursor) -> bool
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		c == Kind::Delim && matches!(c.token().char(), Some('+' | '-'))
	}
}

impl<'a> Parse<'a> for CalcSumOp {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let c = p.peek_n(1);
		if !Self::peek(p, c) {
			Err(Diagnostic::new(c, Diagnostic::unexpected))?;
		}
		let delim = p.parse::<T![Delim]>()?;
		// Force EnforceBefore | EnforceAfter so the sink always emits the
		// spec-required whitespace around `+`/`-` in <calc-sum>, even if the
		// source omitted it. https://www.w3.org/TR/css-values-4/#calc-syntax
		let rules = AssociatedWhitespaceRules::EnforceBefore | AssociatedWhitespaceRules::EnforceAfter;
		Ok(Self(delim.with_associated_whitespace(rules)))
	}
}

impl ToCursors for CalcSumOp {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.0, s);
	}
}

impl ToSpan for CalcSumOp {
	fn to_span(&self) -> Span {
		self.0.to_span()
	}
}

impl SemanticEq for CalcSumOp {
	fn semantic_eq(&self, other: &Self) -> bool {
		self.0.semantic_eq(&other.0)
	}
}

impl<'a> Parse<'a> for CalcProduct<'a> {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let head = p.parse::<CalcValue>()?;
		let mut tail = Vec::new_in(p.bump());
		while <CalcProductOp>::peek(p, p.peek_n(1)) {
			let op = p.parse::<CalcProductOp>()?;
			let rhs = p.parse::<CalcValue>()?;
			tail.push((op, rhs));
		}
		Ok(Self { head, tail })
	}
}

impl<'a> ToCursors for CalcProduct<'a> {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.head, s);
		for (op, rhs) in &self.tail {
			ToCursors::to_cursors(op, s);
			ToCursors::to_cursors(rhs, s);
		}
	}
}

impl<'a> ToSpan for CalcProduct<'a> {
	fn to_span(&self) -> Span {
		match self.tail.last() {
			Some((_, last)) => self.head.to_span() + last.to_span(),
			None => self.head.to_span(),
		}
	}
}

impl<'a> SemanticEq for CalcProduct<'a> {
	fn semantic_eq(&self, other: &Self) -> bool {
		if !self.head.semantic_eq(&other.head) || self.tail.len() != other.tail.len() {
			return false;
		}
		self.tail
			.iter()
			.zip(other.tail.iter())
			.all(|((op_a, rhs_a), (op_b, rhs_b))| op_a.0.semantic_eq(&op_b.0) && rhs_a.semantic_eq(rhs_b))
	}
}

impl<'a> Peek<'a> for CalcProductOp {
	const PEEK_KINDSET: KindSet = KindSet::new(&[Kind::Delim]);

	fn peek<I>(_p: &Parser<'a, I>, c: Cursor) -> bool
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		c == Kind::Delim && matches!(c.token().char(), Some('*' | '/'))
	}
}

impl<'a> Parse<'a> for CalcProductOp {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let c = p.peek_n(1);
		if !Self::peek(p, c) {
			Err(Diagnostic::new(c, Diagnostic::unexpected))?;
		}
		let delim = p.parse::<T![Delim]>()?;
		Ok(Self(delim))
	}
}

impl ToCursors for CalcProductOp {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.0, s);
	}
}

impl ToSpan for CalcProductOp {
	fn to_span(&self) -> Span {
		self.0.to_span()
	}
}

impl SemanticEq for CalcProductOp {
	fn semantic_eq(&self, other: &Self) -> bool {
		self.0.semantic_eq(&other.0)
	}
}

impl<'a> Peek<'a> for CalcValue<'a> {
	const PEEK_KINDSET: KindSet =
		KindSet::new(&[Kind::Number, Kind::Dimension, Kind::Ident, Kind::Function, Kind::LeftParen]);
}

impl<'a> Parse<'a> for CalcValue<'a> {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let c = p.peek_n(1);
		if <T![Number]>::peek(p, c) {
			Ok(Self::Number(p.parse::<T![Number]>()?))
		} else if <T![Dimension]>::peek(p, c) {
			Ok(Self::Dimension(p.parse::<T![Dimension]>()?))
		} else if <T![Ident]>::peek(p, c) {
			Ok(Self::Ident(p.parse::<T![Ident]>()?))
		} else if <CalcExpression<'a>>::peek(p, c) {
			let inner = p.parse::<CalcExpression<'a>>()?;
			Ok(Self::Calc(Box::new(inner)))
		} else if <T![Function]>::peek(p, c) {
			Ok(Self::Function(p.parse::<FunctionBlock>()?))
		} else if <T!['(']>::peek(p, c) {
			Ok(Self::Parens(p.parse::<CalcParens>()?))
		} else {
			Err(Diagnostic::new(c, Diagnostic::unexpected))?
		}
	}
}

impl<'a> ToCursors for CalcValue<'a> {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		match self {
			Self::Number(t) => ToCursors::to_cursors(t, s),
			Self::Dimension(t) => ToCursors::to_cursors(t, s),
			Self::Ident(t) => ToCursors::to_cursors(t, s),
			Self::Calc(t) => ToCursors::to_cursors(&**t, s),
			Self::Function(t) => ToCursors::to_cursors(t, s),
			Self::Parens(t) => ToCursors::to_cursors(t, s),
		}
	}
}

impl<'a> ToSpan for CalcValue<'a> {
	fn to_span(&self) -> Span {
		match self {
			Self::Number(t) => t.to_span(),
			Self::Dimension(t) => t.to_span(),
			Self::Ident(t) => t.to_span(),
			Self::Calc(t) => t.to_span(),
			Self::Function(t) => t.to_span(),
			Self::Parens(t) => t.to_span(),
		}
	}
}

impl<'a> SemanticEq for CalcValue<'a> {
	fn semantic_eq(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Number(a), Self::Number(b)) => a.semantic_eq(b),
			(Self::Dimension(a), Self::Dimension(b)) => a.semantic_eq(b),
			(Self::Ident(a), Self::Ident(b)) => a.semantic_eq(b),
			(Self::Calc(a), Self::Calc(b)) => a.semantic_eq(b),
			(Self::Function(a), Self::Function(b)) => a.semantic_eq(b),
			(Self::Parens(a), Self::Parens(b)) => a.semantic_eq(b),
			_ => false,
		}
	}
}

impl<'a> Peek<'a> for CalcParens<'a> {
	const PEEK_KINDSET: KindSet = KindSet::new(&[Kind::LeftParen]);
}

impl<'a> Parse<'a> for CalcParens<'a> {
	fn parse<I>(p: &mut Parser<'a, I>) -> ParserResult<Self>
	where
		I: Iterator<Item = Cursor> + Clone,
	{
		let open = p.parse::<T!['(']>()?;
		let sum = p.parse::<CalcSum>()?;
		let close = p.parse::<T![')']>()?;
		Ok(Self { open, sum: Box::new(sum), close })
	}
}

impl<'a> ToCursors for CalcParens<'a> {
	fn to_cursors(&self, s: &mut impl CursorSink) {
		ToCursors::to_cursors(&self.open, s);
		ToCursors::to_cursors(&*self.sum, s);
		ToCursors::to_cursors(&self.close, s);
	}
}

impl<'a> ToSpan for CalcParens<'a> {
	fn to_span(&self) -> Span {
		self.open.to_span() + self.close.to_span()
	}
}

impl<'a> SemanticEq for CalcParens<'a> {
	fn semantic_eq(&self, other: &Self) -> bool {
		self.sum.semantic_eq(&other.sum)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{EmptyAtomSet, test_helpers::*};

	#[test]
	fn test_peek_only_math_functions() {
		// Positive peek cases are covered by `assert_parse!` below — if parse succeeds,
		// peek returned true. Here we just verify that non-math functions don't trip
		// the peek so the dispatch in `ComponentValue::parse` stays correct.
		assert_peek_false!(EmptyAtomSet::ATOMS, CalcExpression, "var(--x)");
		assert_peek_false!(EmptyAtomSet::ATOMS, CalcExpression, "url(foo)");
		assert_peek_false!(EmptyAtomSet::ATOMS, CalcExpression, "rgb(0,0,0)");
	}

	#[test]
	fn test_parse_calc_simple() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(1px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(5px + 1px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(5px - 1px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(5px * 2)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(5px / 2)");
	}

	#[test]
	fn test_parse_calc_precedence() {
		// `1 + 2 * 3` is `1 + (2 * 3)`: CalcProduct nests inside CalcSum.
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(1 + 2 * 3)");
	}

	#[test]
	fn test_parse_calc_parens() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc((5px - 1px) + 2px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc((var(--x) / -2) - 5px)");
	}

	#[test]
	fn test_parse_calc_nested_calc() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(calc(1px) + 2px)");
	}

	#[test]
	fn test_parse_calc_with_var() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(var(--x) + 1px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "calc(var(--col-gap) / 2 + var(--date-col))");
	}

	#[test]
	fn test_parse_min_max_clamp() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "min(5px, 10px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "max(5px, 10px)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "clamp(1px, 5%, 10px)");
	}

	#[test]
	fn test_parse_mod_rem() {
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "mod(5, 2)");
		assert_parse!(EmptyAtomSet::ATOMS, CalcExpression, "rem(5, 2)");
	}
}
