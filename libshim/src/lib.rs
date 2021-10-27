#![feature(allocator_api)]

use acollections::{ABox, AClone, AHashMap, AVec};
use lexical_core::FormattedSize;
use std::alloc::AllocError;
use std::fmt::Debug;
use std::ops::Deref;
use tally_ho::{Collector, Gc, Manage};

#[derive(Debug)]
pub enum ShimError {
    PureParseError(PureParseError),
    AllocError(AllocError),
    Other(&'static [u8]),
}

impl From<ParseError> for ShimError {
    fn from(err: ParseError) -> ShimError {
        match err {
            ParseError::AllocError(err) => ShimError::AllocError(err),
            ParseError::PureParseError(err) => ShimError::PureParseError(err),
        }
    }
}

impl From<AllocError> for ShimError {
    fn from(err: AllocError) -> ShimError {
        ShimError::AllocError(err)
    }
}

// TODO: Forcing the allocator to be copyable doesn't seem right in the general
// case of having sized allocators. It's perfect for zero-sized ones though!
pub trait Allocator: std::alloc::Allocator + Copy + std::fmt::Debug {}

#[derive(Debug, Copy, Clone)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Token<'a> {
    Unknown(&'a [u8]),
    StringLiteral(&'a [u8]),
    UnclosedStringLiteral(&'a [u8]),
    BoolLiteral(bool),
    IntLiteral(i128),
    FloatLiteral(f64),
    Identifier(&'a [u8]),
    LeftParen,
    RightParen,
    LeftCurly,
    RightCurly,
    LeftAngle,
    RightAngle,
    LeftSquare,
    RightSquare,
    Dot,
    Plus,
    Minus,
    Sub,
    Star,
    DoubleStar,
    Slash,
    Colon,
    DoubleColon,
    Semicolon,
    Arrow,
    // TODO: plus_equals, etc.
    DoubleEqual,
    Equal,
    Bang,
    BangEqual,
    Comma,
    Gte,
    Lte,
    AndKeyword,
    AsKeyword,
    BreakKeyword,
    ContinueKeyword,
    ElseKeyword,
    EnumKeyword,
    FnKeyword,
    ForKeyword,
    IfKeyword,
    OrKeyword,
    ReturnKeyword,
    StructKeyword,
    UseKeyword,
    LetKeyword,
    WhileKeyword,
    TrueKeyword,
    FalseKeyword,
    EOF,
}

struct TokenStream<'a> {
    text: &'a [u8],
    idx: usize,
    cached_next: Option<(Token<'a>, usize)>,
}

impl<'a> TokenStream<'a> {
    pub fn new(text: &'a [u8]) -> Self {
        TokenStream {
            text,
            idx: 0,
            cached_next: None,
        }
    }

    fn peek_inc(&mut self) -> (Token, usize) {
        if let Some(pair) = self.cached_next {
            return pair;
        }

        // Iterate over whitespace until we find a token to return. We accumulate
        // the number of bytes we consume in inc.
        let mut inc = 0;
        loop {
            let chars_left: isize = self.text.len() as isize - self.idx as isize - inc as isize;
            let next = if chars_left <= 0 {
                (Token::EOF, 0)
            } else {
                let multichar_ok = chars_left > 2;
                match self.text[self.idx + inc] {
                    b' ' | b'\t' | b'\r' | b'\n' => {
                        // TODO: line_numbers
                        inc += 1;
                        continue;
                    }
                    b'(' => (Token::LeftParen, inc + 1),
                    b')' => (Token::RightParen, inc + 1),
                    b'{' => (Token::LeftCurly, inc + 1),
                    b'}' => (Token::RightCurly, inc + 1),
                    b'[' => (Token::LeftSquare, inc + 1),
                    b']' => (Token::RightSquare, inc + 1),
                    b'.' => (Token::Dot, inc + 1),
                    b',' => (Token::Comma, inc + 1),
                    b'+' => (Token::Plus, inc + 1),
                    b';' => (Token::Semicolon, inc + 1),
                    b':' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b':' {
                            (Token::DoubleColon, inc + 2)
                        } else {
                            (Token::Colon, inc + 1)
                        }
                    }
                    b'<' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'=' {
                            (Token::Lte, inc + 2)
                        } else {
                            (Token::LeftAngle, inc + 1)
                        }
                    }
                    b'>' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'=' {
                            (Token::Gte, inc + 2)
                        } else {
                            (Token::RightAngle, inc + 1)
                        }
                    }
                    b'-' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'>' {
                            (Token::Arrow, inc + 2)
                        } else {
                            (Token::Minus, inc + 1)
                        }
                    }
                    b'*' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'*' {
                            (Token::DoubleStar, inc + 2)
                        } else {
                            (Token::Star, inc + 1)
                        }
                    }
                    b'=' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'=' {
                            (Token::DoubleEqual, inc + 2)
                        } else {
                            (Token::Equal, inc + 1)
                        }
                    }
                    b'!' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'=' {
                            (Token::BangEqual, inc + 2)
                        } else {
                            (Token::Bang, inc + 1)
                        }
                    }
                    b'/' => {
                        if multichar_ok && self.text[self.idx + inc + 1] == b'/' {
                            // This is a comment. Consume everything until we reach a newline
                            while self.idx + inc < self.text.len()
                                && self.text[self.idx + inc] != b'\n'
                            {
                                inc += 1;
                            }
                            continue;
                        } else {
                            (Token::Slash, inc + 1)
                        }
                    }
                    b'"' => {
                        inc += 1;
                        let start_idx = self.idx + inc;
                        // Keep going if:
                        // - There's more text, and
                        //   - It's not a quote, or
                        //   - The quote is escaped
                        // NOTE: the escaped quote will be handled by the parser
                        while self.idx + inc < self.text.len()
                            && (self.text[self.idx + inc - 1] == b'\\'
                                || self.text[self.idx + inc] != b'"')
                        {
                            inc += 1;
                        }

                        if self.idx + inc == self.text.len() {
                            // Someone forgot a closing quote :)
                            (
                                Token::UnclosedStringLiteral(&self.text[self.idx + 1..]),
                                inc,
                            )
                        } else {
                            // Consume the closing quote
                            inc += 1;
                            (
                                // Subtract 1 from `inc` to _not_ include the closing quote
                                Token::StringLiteral(&self.text[start_idx..self.idx + inc - 1]),
                                inc,
                            )
                        }
                    }
                    n @ b'0'..=b'9' => {
                        #[derive(Copy, Clone)]
                        enum NumericParseState {
                            LeadingZero,
                            Int,
                            Binary,
                            Hex,
                            Float,
                        }

                        let mut parse_state = if n == 0 {
                            NumericParseState::LeadingZero
                        } else {
                            NumericParseState::Int
                        };

                        inc += 1;

                        let mut float_decimal_power = -1;
                        let mut float_value: f64 = 0.0;
                        let mut int_value: i128 = (n - b'0') as i128;
                        while self.idx + inc < self.text.len() {
                            match (parse_state, self.text[self.idx + inc]) {
                                (NumericParseState::LeadingZero, b'x') => {
                                    parse_state = NumericParseState::Hex;
                                }
                                (NumericParseState::LeadingZero, b'b') => {
                                    parse_state = NumericParseState::Binary;
                                }
                                (NumericParseState::LeadingZero | NumericParseState::Int, b'.') => {
                                    float_value = int_value as f64;
                                    parse_state = NumericParseState::Float;
                                }
                                (NumericParseState::LeadingZero, _) => {
                                    break;
                                }
                                (NumericParseState::Int, n) => {
                                    if matches!(n, b'0'..=b'9') {
                                        int_value *= 10;
                                        int_value += (n - b'0') as i128;
                                    } else {
                                        break;
                                    }
                                }
                                (NumericParseState::Binary, n) => {
                                    if matches!(n, b'0' | b'1') {
                                        int_value *= 2;
                                        int_value += (n - b'0') as i128;
                                    } else {
                                        break;
                                    }
                                }
                                (NumericParseState::Hex, n) => {
                                    if matches!(n, b'0'..=b'9') {
                                        int_value *= 16;
                                        int_value += (n - b'0') as i128;
                                    } else if matches!(n, b'a'..=b'f') {
                                        int_value *= 16;
                                        int_value += (n - b'a' + 10) as i128;
                                    } else if matches!(n, b'A'..=b'F') {
                                        int_value *= 16;
                                        int_value += (n - b'A' + 10) as i128;
                                    } else {
                                        break;
                                    }
                                }
                                (NumericParseState::Float, n) => {
                                    // TODO: support 'E' for scientific notation
                                    if matches!(n, b'0'..=b'9') {
                                        float_value += ((n - b'0') as f64)
                                            * (10.0f64).powi(float_decimal_power);
                                        float_decimal_power -= 1;
                                    } else {
                                        break;
                                    }
                                }
                            }
                            inc += 1;
                        }

                        match parse_state {
                            NumericParseState::Float => (Token::FloatLiteral(float_value), inc),
                            _ => (Token::IntLiteral(int_value), inc),
                        }
                    }
                    b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                        let start_idx = self.idx + inc;

                        while self.idx + inc < self.text.len()
                            && matches!(self.text[self.idx + inc], b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'0'..=b'9' )
                        {
                            inc += 1;
                        }
                        let slice = &self.text[start_idx..self.idx + inc];
                        match slice {
                            b"and" => (Token::AndKeyword, inc),
                            b"as" => (Token::AsKeyword, inc),
                            b"break" => (Token::BreakKeyword, inc),
                            b"continue" => (Token::ContinueKeyword, inc),
                            b"else" => (Token::ElseKeyword, inc),
                            b"enum" => (Token::EnumKeyword, inc),
                            b"fn" => (Token::FnKeyword, inc),
                            b"for" => (Token::ForKeyword, inc),
                            b"if" => (Token::IfKeyword, inc),
                            b"or" => (Token::OrKeyword, inc),
                            b"return" => (Token::ReturnKeyword, inc),
                            b"struct" => (Token::StructKeyword, inc),
                            b"use" => (Token::UseKeyword, inc),
                            b"let" => (Token::LetKeyword, inc),
                            b"while" => (Token::WhileKeyword, inc),
                            b"true" => (Token::BoolLiteral(true), inc),
                            b"false" => (Token::BoolLiteral(false), inc),
                            _ => (Token::Identifier(slice), inc),
                        }
                    }
                    _ => (
                        Token::Unknown(&self.text[self.idx + inc..self.idx + inc + 1]),
                        1,
                    ),
                }
            };

            self.cached_next = Some(next);

            return next;
        }
    }

    pub fn peek(&mut self) -> Token {
        self.peek_inc().0
    }

    fn unchecked_advance(&mut self, inc: usize) {
        self.cached_next = None;
        self.idx += inc
    }

    pub fn advance(&mut self) {
        let (_, inc) = self.peek_inc();
        self.unchecked_advance(inc);
    }

    pub fn matches(&mut self, check_token: Token) -> bool {
        let (next_token, inc) = self.peek_inc();
        if std::mem::discriminant(&check_token) == std::mem::discriminant(&next_token) {
            self.unchecked_advance(inc);
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub struct CallExpr<A: Allocator> {
    func: ABox<Expression<A>, A>,
    args: AVec<Expression<A>, A>,
}

impl<A: Allocator> AClone for CallExpr<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        Ok(CallExpr {
            func: ABox::aclone(&self.func)?,
            args: self.args.aclone()?,
        })
    }
}

#[derive(Debug)]
pub enum Expression<A: Allocator> {
    Identifier(AVec<u8, A>),
    BoolLiteral(bool),
    IntLiteral(i128),
    FloatLiteral(f64),
    StringLiteral(AVec<u8, A>),
    Binary(BinaryOp, ABox<Expression<A>, A>, ABox<Expression<A>, A>),
    Call(CallExpr<A>),
    BlockCall(ABox<Expression<A>, A>, Block<A>),
    Get(ABox<Expression<A>, A>, AVec<u8, A>),
}

impl<A: Allocator> AClone for Expression<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        let res = match self {
            Expression::Identifier(vec) => Expression::Identifier(vec.aclone()?),
            Expression::BoolLiteral(b) => Expression::BoolLiteral(*b),
            Expression::IntLiteral(i) => Expression::IntLiteral(*i),
            Expression::FloatLiteral(f) => Expression::FloatLiteral(*f),
            Expression::StringLiteral(vec) => Expression::StringLiteral(vec.aclone()?),
            Expression::Binary(op, expr_a, expr_b) => {
                Expression::Binary(*op, ABox::aclone(expr_a)?, ABox::aclone(expr_b)?)
            }
            Expression::Call(cexpr) => Expression::Call(cexpr.aclone()?),
            Expression::BlockCall(obj, block) => {
                Expression::BlockCall(obj.aclone()?, block.aclone()?)
            }
            Expression::Get(gexpr, prop) => Expression::Get(gexpr.aclone()?, prop.aclone()?),
        };

        Ok(res)
    }
}

#[derive(Debug)]
pub struct Block<A: Allocator> {
    stmts: AVec<Statement<A>, A>,
}

impl<A: Allocator> AClone for Block<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        Ok(Block {
            stmts: self.stmts.aclone()?,
        })
    }
}

#[derive(Debug)]
pub struct IfStatement<A: Allocator> {
    predicate: Expression<A>,
    if_block: Block<A>,
    else_block: Option<Block<A>>,
}

impl<A: Allocator> AClone for IfStatement<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        Ok(IfStatement {
            predicate: self.predicate.aclone()?,
            if_block: self.if_block.aclone()?,
            else_block: if let Some(block) = &self.else_block {
                Some(block.aclone()?)
            } else {
                None
            },
        })
    }
}

#[derive(Debug)]
pub struct FnDef<A: Allocator> {
    name: AVec<u8, A>,
    args: AVec<AVec<u8, A>, A>,
    block: Block<A>,
}

impl<A: Allocator> FnDef<A> {
    fn new(name: AVec<u8, A>, args: AVec<AVec<u8, A>, A>, block: Block<A>) -> Self {
        Self { name, args, block }
    }
}

impl<A: Allocator> AClone for FnDef<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        Ok(FnDef {
            name: self.name.aclone()?,
            args: self.args.aclone()?,
            block: self.block.aclone()?,
        })
    }
}

#[derive(Debug)]
pub enum Statement<A: Allocator> {
    Expression(Expression<A>),
    Declaration(AVec<u8, A>, Expression<A>),
    Assignment(AVec<u8, A>, Expression<A>),
    Block(Block<A>),
    IfStatement(IfStatement<A>),
    WhileStatement(Expression<A>, Block<A>),
    FnDef(FnDef<A>),
    StructDef(AVec<u8, A>, AVec<AVec<u8, A>, A>, AVec<FnDef<A>, A>),
    BreakStatement,
    ContinueStatement,
    ReturnStatement(Expression<A>),
}

impl<A: Allocator> AClone for Statement<A> {
    fn aclone(&self) -> Result<Self, AllocError> {
        let res = match self {
            Statement::Expression(expr) => Statement::Expression(expr.aclone()?),
            Statement::Declaration(name, expr) => {
                Statement::Declaration(name.aclone()?, expr.aclone()?)
            }
            Statement::Assignment(name, expr) => {
                Statement::Assignment(name.aclone()?, expr.aclone()?)
            }
            Statement::Block(block) => Statement::Block(block.aclone()?),
            Statement::IfStatement(if_stmt) => Statement::IfStatement(if_stmt.aclone()?),
            Statement::WhileStatement(predicate, block) => {
                Statement::WhileStatement(predicate.aclone()?, block.aclone()?)
            }
            Statement::FnDef(def) => Statement::FnDef(def.aclone()?),
            Statement::StructDef(name, members, methods) => {
                Statement::StructDef(name.aclone()?, members.aclone()?, methods.aclone()?)
            }
            Statement::BreakStatement => Statement::BreakStatement,
            Statement::ContinueStatement => Statement::ContinueStatement,
            Statement::ReturnStatement(expr) => Statement::ReturnStatement(expr.aclone()?),
        };

        Ok(res)
    }
}

#[derive(Debug)]
pub enum PureParseError {
    UnexpectedToken,
    Generic(&'static [u8]),
}

pub enum ParseError {
    AllocError(AllocError),
    PureParseError(PureParseError),
}

impl From<PureParseError> for ParseError {
    fn from(err: PureParseError) -> ParseError {
        ParseError::PureParseError(err)
    }
}

impl From<AllocError> for ParseError {
    fn from(err: AllocError) -> ParseError {
        ParseError::AllocError(err)
    }
}

fn parse_equality<'a, A: Allocator>(
    tokens: &mut TokenStream,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_binary(
        tokens,
        &[
            (Token::DoubleEqual, BinaryOp::Eq),
            (Token::BangEqual, BinaryOp::Neq),
        ],
        parse_comparison,
        in_predicate,
        allocator,
    )
}

fn parse_comparison<'a, A: Allocator>(
    tokens: &mut TokenStream,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_binary(
        tokens,
        &[
            (Token::LeftAngle, BinaryOp::Lt),
            (Token::Lte, BinaryOp::Lte),
            (Token::RightAngle, BinaryOp::Gt),
            (Token::Gte, BinaryOp::Gte),
        ],
        parse_term,
        in_predicate,
        allocator,
    )
}

fn parse_term<'a, A: Allocator>(
    tokens: &mut TokenStream,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_binary(
        tokens,
        &[(Token::Plus, BinaryOp::Add), (Token::Minus, BinaryOp::Sub)],
        parse_factor,
        in_predicate,
        allocator,
    )
}

fn parse_factor<'a, A: Allocator>(
    tokens: &mut TokenStream,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_binary(
        tokens,
        &[(Token::Star, BinaryOp::Mul), (Token::Slash, BinaryOp::Div)],
        parse_call,
        in_predicate,
        allocator,
    )
}

fn parse_call<'a, A: Allocator>(
    tokens: &mut TokenStream,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    let mut expr = parse_primary(tokens, allocator)?;
    while tokens.peek() != Token::EOF {
        match (tokens.peek(), in_predicate) {
            (Token::LeftParen, _) => {
                tokens.advance();
                let args = parse_args(tokens, allocator)?;

                if !tokens.matches(Token::RightParen) {
                    return Err(
                        PureParseError::Generic(b"Right paren did not follow arglist").into(),
                    );
                }

                expr = Expression::Call(CallExpr {
                    func: ABox::new(expr, allocator)?,
                    args: args,
                });
            }
            (Token::Dot, _) => {
                tokens.advance();
                if let Token::Identifier(ident) = tokens.peek() {
                    let mut property = AVec::new(allocator);
                    property.extend_from_slice(ident)?;

                    tokens.advance();

                    expr = Expression::Get(ABox::new(expr, allocator)?, property);
                } else {
                    return Err(PureParseError::Generic(b"Expected ident after dot").into());
                }
            }
            (Token::LeftCurly, false) => {
                let block = parse_block(tokens, allocator)?;
                expr = Expression::BlockCall(ABox::new(expr, allocator)?, block);
            }
            _ => {
                break;
            }
        }
    }

    Ok(expr)
}

fn parse_args<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<AVec<Expression<A>, A>, ParseError> {
    let mut args = AVec::new(allocator);
    while tokens.peek() != Token::EOF && tokens.peek() != Token::RightParen {
        args.push(parse_expression(tokens, allocator)?)?;
        if tokens.matches(Token::Comma) {
            continue;
        }
        break;
    }

    Ok(args)
}

fn parse_primary<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    match tokens.peek() {
        Token::BoolLiteral(b) => {
            tokens.advance();
            Ok(Expression::BoolLiteral(b))
        }
        Token::IntLiteral(i) => {
            tokens.advance();
            Ok(Expression::IntLiteral(i))
        }
        Token::FloatLiteral(f) => {
            tokens.advance();
            Ok(Expression::FloatLiteral(f))
        }
        Token::StringLiteral(s) => {
            let mut new_str = AVec::new(allocator);
            let mut slash = false;
            for c in s {
                new_str.push(match (c, slash) {
                    (b'\\', false) => {
                        slash = true;
                        continue;
                    }
                    (b'n', true) => b'\n',
                    (b't', true) => b'\t',
                    (b'r', true) => b'\r',
                    _ => *c,
                })?;
                slash = false;
            }

            tokens.advance();
            Ok(Expression::StringLiteral(new_str))
        }
        Token::Identifier(ident) => {
            let mut vec = AVec::new(allocator);
            vec.extend_from_slice(ident)?;

            tokens.advance();
            Ok(Expression::Identifier(vec))
        }
        token => {
            if cfg!(debug_assertions) {
                println!("Token: {:?}", token);
            }

            Err(PureParseError::Generic(b"Unknown token when parsing primary").into())
        }
    }
}

fn parse_binary<'a, A: Allocator>(
    tokens: &mut TokenStream,
    op_table: &[(Token, BinaryOp)],
    next: fn(&mut TokenStream, bool, A) -> Result<Expression<A>, ParseError>,
    in_predicate: bool,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    let mut expr = next(tokens, in_predicate, allocator)?;
    while tokens.peek() != Token::EOF {
        let token = tokens.peek();
        if let Some(op) = op_table
            .iter()
            .find(|(table_token, _)| token == *table_token)
            .map(|(_, op)| op)
        {
            // Consume the token we peeked
            tokens.advance();

            let right_expr = next(tokens, in_predicate, allocator)?;
            expr = Expression::Binary(
                *op,
                ABox::new(expr, allocator)?,
                ABox::new(right_expr, allocator)?,
            );
        } else {
            break;
        }
    }

    Ok(expr)
}

fn parse_script<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Block<A>, ParseError> {
    // TODO: shebang
    let block = parse_block_open(tokens, allocator)?;
    if tokens.matches(Token::EOF) {
        Ok(block)
    } else {
        Err(PureParseError::Generic(b"Unconsumed tokens!").into())
    }
}

// NOTE: consumes open and closing curly
fn parse_block<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Block<A>, ParseError> {
    if !tokens.matches(Token::LeftCurly) {
        if cfg!(debug_assertions) {
            let token = tokens.peek();
            println!("Token: {:?}", token);
        }
        return Err(PureParseError::Generic(b"Block does not start with opening '{'").into());
    }

    let block = parse_block_open(tokens, allocator)?;

    if !tokens.matches(Token::RightCurly) {
        return Err(PureParseError::Generic(b"Block does not end with closing '}'").into());
    }

    Ok(block)
}

fn parse_block_open<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Block<A>, ParseError> {
    let mut stmts = AVec::new(allocator);
    while tokens.peek() != Token::EOF && tokens.peek() != Token::RightCurly {
        let stmt = parse_statement(tokens, allocator)?;
        stmts.push(stmt)?;
    }

    Ok(Block { stmts })
}

fn parse_if<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Statement<A>, ParseError> {
    if tokens.matches(Token::IfKeyword) {
        let predicate = parse_predicate(tokens, allocator)?;
        let if_block = parse_block(tokens, allocator)?;

        let else_block = if tokens.matches(Token::ElseKeyword) {
            if tokens.peek() == Token::IfKeyword {
                let mut stmts = AVec::new(allocator);
                stmts.push(parse_if(tokens, allocator)?)?;
                Some(Block { stmts })
            } else {
                Some(parse_block(tokens, allocator)?)
            }
        } else {
            None
        };

        Ok(Statement::IfStatement(IfStatement {
            predicate,
            if_block,
            else_block,
        }))
    } else {
        Err(PureParseError::Generic(b"(Internal) expected to match if").into())
    }
}

fn parse_fn<A: Allocator>(tokens: &mut TokenStream, allocator: A) -> Result<FnDef<A>, ParseError> {
    if !tokens.matches(Token::FnKeyword) {
        return Err(PureParseError::Generic(b"Expected 'fn' when parsing function").into());
    }

    if let Token::Identifier(name) = tokens.peek() {
        let mut name_vec = AVec::new(allocator);
        name_vec.extend_from_slice(&name)?;

        tokens.advance();
        if !tokens.matches(Token::LeftParen) {
            return Err(PureParseError::Generic(b"Missing paren after fn identifier").into());
        }

        let mut arg_names = AVec::new(allocator);
        while tokens.peek() != Token::RightParen && !tokens.matches(Token::EOF) {
            if let Token::Identifier(arg_name) = tokens.peek() {
                let arg_name = AVec::from_slice(&arg_name, allocator)?;
                arg_names.push(arg_name)?;
            } else {
                return Err(PureParseError::Generic(b"Expected identifier in fn arg list").into());
            }

            tokens.advance();

            if tokens.matches(Token::Comma) {
                continue;
            }
            break;
        }
        if !tokens.matches(Token::RightParen) {
            return Err(PureParseError::Generic(b"Expected right paren after fn arg list").into());
        }

        let block = parse_block(tokens, allocator)?;

        Ok(FnDef::new(name_vec, arg_names, block))
    } else {
        Err(PureParseError::Generic(b"Missing identifier after fn").into())
    }
}

fn parse_statement<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Statement<A>, ParseError> {
    if tokens.peek() == Token::LeftCurly {
        let block = parse_block(tokens, allocator)?;

        // TODO: if there's a semicolon this was actually a block _expression_

        Ok(Statement::Block(block))
    } else if tokens.matches(Token::LetKeyword) {
        if let Token::Identifier(name) = tokens.peek() {
            let mut name_vec = AVec::new(allocator);
            name_vec.extend_from_slice(&name)?;

            tokens.advance();
            if !tokens.matches(Token::Equal) {
                return Err(PureParseError::Generic(b"Missing equal in declaration").into());
            }
            let expr = parse_expression(tokens, allocator)?;

            if !tokens.matches(Token::Semicolon) {
                return Err(PureParseError::Generic(b"Missing semicolon after declaration").into());
            }

            Ok(Statement::Declaration(name_vec, expr))
        } else {
            return Err(PureParseError::Generic(b"Missing identifier after let").into());
        }
    } else if tokens.matches(Token::StructKeyword) {
        if let Token::Identifier(name) = tokens.peek() {
            let mut name_vec = AVec::new(allocator);
            name_vec.extend_from_slice(&name)?;

            tokens.advance();
            if !tokens.matches(Token::LeftCurly) {
                return Err(PureParseError::Generic(b"Missing '{' in struct declaration").into());
            }

            let mut members = AVec::new(allocator);
            while tokens.peek() != Token::EOF
                && tokens.peek() != Token::RightCurly
                && tokens.peek() != Token::FnKeyword
            {
                if let Token::Identifier(name) = tokens.peek() {
                    let mut name_vec = AVec::new(allocator);
                    name_vec.extend_from_slice(&name)?;

                    tokens.advance();

                    members.push(name_vec)?;
                } else {
                    return Err(PureParseError::Generic(b"Expected ident in struct def").into());
                }

                // This should (?) handle trailing commas
                if !tokens.matches(Token::Comma) {
                    break;
                }
            }

            let mut methods = AVec::new(allocator);
            while tokens.peek() == Token::FnKeyword {
                let fn_def = parse_fn(tokens, allocator)?;
                methods.push(fn_def)?;
            }

            if !tokens.matches(Token::RightCurly) {
                return Err(PureParseError::Generic(b"Missing '}' at end of struct def").into());
            }

            Ok(Statement::StructDef(name_vec, members, methods))
        } else {
            return Err(PureParseError::Generic(b"Missing identifier after struct").into());
        }
    } else if tokens.peek() == Token::IfKeyword {
        Ok(parse_if(tokens, allocator)?)
    } else if tokens.matches(Token::WhileKeyword) {
        let predicate = parse_predicate(tokens, allocator)?;
        let block = parse_block(tokens, allocator)?;
        Ok(Statement::WhileStatement(predicate, block))
    } else if tokens.matches(Token::BreakKeyword) {
        if !tokens.matches(Token::Semicolon) {
            return Err(PureParseError::Generic(b"Missing semicolon after break").into());
        }
        Ok(Statement::BreakStatement)
    } else if tokens.matches(Token::ContinueKeyword) {
        if !tokens.matches(Token::Semicolon) {
            return Err(PureParseError::Generic(b"Missing semicolon after continue").into());
        }
        Ok(Statement::ContinueStatement)
    } else if tokens.matches(Token::ReturnKeyword) {
        let expr = parse_expression(tokens, allocator)?;
        if !tokens.matches(Token::Semicolon) {
            return Err(PureParseError::Generic(b"Missing semicolon after return").into());
        }
        Ok(Statement::ReturnStatement(expr))
    } else if tokens.peek() == Token::FnKeyword {
        Ok(Statement::FnDef(parse_fn(tokens, allocator)?))
    } else {
        let expr = parse_expression(tokens, allocator)?;
        let stmt = match (expr, tokens.peek()) {
            (Expression::Identifier(ident), Token::Equal) => {
                tokens.advance();
                let expr = parse_expression(tokens, allocator)?;

                Statement::Assignment(ident, expr)
            }
            (expr, _) => Statement::Expression(expr),
        };

        if !tokens.matches(Token::Semicolon) {
            return Err(PureParseError::Generic(b"Missing semicolon after expression").into());
        }

        Ok(stmt)
    }
}

fn parse_expression<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_equality(tokens, false, allocator)
}

fn parse_predicate<'a, A: Allocator>(
    tokens: &mut TokenStream,
    allocator: A,
) -> Result<Expression<A>, ParseError> {
    parse_equality(tokens, true, allocator)
}

enum InstanceProp<A: Allocator> {
    Slot(usize),
    Method(Gc<ShimValue<A>>),
}

pub struct StructDef<A: Allocator> {
    props: AHashMap<AVec<u8, A>, InstanceProp<A>, A>,
}

impl<A: Allocator> StructDef<A> {
    fn instance_prop(&self, name: &AVec<u8, A>) -> Option<&InstanceProp<A>> {
        self.props.get(name)
    }

    fn instance_members(&self) -> impl Iterator<Item = &AVec<u8, A>> {
        self.props.iter().filter_map(|entry| {
            if matches!(entry.value(), InstanceProp::Slot(_)) {
                Some(entry.key())
            } else {
                None
            }
        })
    }
}

pub struct Struct<A: Allocator>(Gc<ShimValue<A>>, AVec<Gc<ShimValue<A>>, A>);

pub enum ShimValue<A: Allocator> {
    // A variant used to replace a previous-valid value after GC
    Freed,
    Unit,
    Bool(bool),
    I128(i128),
    F64(f64),
    SString(AVec<u8, A>),
    SFn(AVec<AVec<u8, A>, A>, Block<A>, Gc<ShimValue<A>>),
    BoundFn(Gc<ShimValue<A>>, Gc<ShimValue<A>>),
    NativeFn(
        Box<
            dyn Fn(
                AVec<Gc<ShimValue<A>>, A>,
                &mut Interpreter<A>,
            ) -> Result<Gc<ShimValue<A>>, ShimError>,
        >,
    ),
    Env(Environment<A>),
    StructDef(StructDef<A>),
    Struct(Struct<A>),
}

impl<A: Allocator> Debug for ShimValue<A> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Self::I128(val) => fmt.write_fmt(format_args!("{}", val)),
            Self::F64(val) => fmt.write_fmt(format_args!("{}", val)),
            Self::Bool(b) => fmt.write_fmt(format_args!("{}", b)),
            Self::Freed => fmt.write_fmt(format_args!("<freed>")),
            Self::Unit => fmt.write_fmt(format_args!("()")),
            Self::SString(s) => fmt.write_fmt(format_args!("{}", std::str::from_utf8(s).unwrap())),
            Self::SFn(..) => fmt.write_fmt(format_args!("<fn>")),
            Self::BoundFn(..) => fmt.write_fmt(format_args!("<bound fn>")),
            Self::NativeFn(..) => fmt.write_fmt(format_args!("<native fn>")),
            Self::Env(..) => fmt.write_fmt(format_args!("<env>")),
            Self::StructDef(..) => fmt.write_fmt(format_args!("<struct def>")),
            Self::Struct(..) => fmt.write_fmt(format_args!("<struct>")),
        }
    }
}

impl<A: Allocator> ShimValue<A> {
    fn as_struct_def(&self) -> Option<&StructDef<A>> {
        match self {
            Self::StructDef(def) => Some(def),
            _ => None,
        }
    }

    fn stringify(&self, allocator: A) -> Result<AVec<u8, A>, AllocError> {
        let mut vec = AVec::new(allocator);
        match self {
            Self::I128(val) => {
                let mut val: i128 = *val;
                if val < 0 {
                    vec.push(b'-')?;
                    // Hopefully this isn't i128::MIN! (since it can't be expressed as a positive)
                    val = val * -1;
                }

                // The largest 128-bit int is 39 characters long
                let mut slice = [b'0'; 39];

                // TODO: check for off-by-one errors in the length
                let mut length = 1;
                for idx in (0..=38).rev() {
                    slice[idx] = b'0' + (val % 10) as u8;
                    val /= 10;
                    if val > 0 {
                        length += 1;
                    }
                }

                vec.extend_from_slice(&slice[39 - length..39])?;
            }
            Self::F64(val) => {
                let mut buffer = [0u8; f64::FORMATTED_SIZE];
                let slice = lexical_core::write(*val, &mut buffer);
                vec.extend_from_slice(&slice)?
            }
            Self::Bool(true) => vec.extend_from_slice(b"true")?,
            Self::Bool(false) => vec.extend_from_slice(b"false")?,
            Self::Freed => vec.extend_from_slice(b"*freed*")?,
            Self::Unit => vec.extend_from_slice(b"()")?,
            Self::SString(s) => vec.extend_from_slice(s)?,
            Self::SFn(..) => vec.extend_from_slice(b"<fn>")?,
            Self::BoundFn(..) => vec.extend_from_slice(b"<bound fn>")?,
            Self::NativeFn(..) => vec.extend_from_slice(b"<native fn>")?,
            Self::Env(..) => vec.extend_from_slice(b"<environment>")?,
            Self::StructDef(..) => vec.extend_from_slice(b"<struct def>")?,
            Self::Struct(..) => vec.extend_from_slice(b"<struct>")?,
        }

        Ok(vec)
    }

    fn is_truthy(&self) -> bool {
        match self {
            Self::I128(val) => *val != 0,
            Self::F64(val) => *val != 0.0,
            Self::Bool(b) => *b,
            Self::Freed => false,
            Self::Unit => false,
            Self::SString(s) => s.len() > 0,
            Self::SFn(..) => true,
            Self::BoundFn(..) => true,
            Self::NativeFn(..) => true,
            Self::Env(..) => true,
            Self::StructDef(..) => true,
            Self::Struct(..) => true,
        }
    }

    fn call(
        &self,
        args: AVec<Gc<ShimValue<A>>, A>,
        interpreter: &mut Interpreter<A>,
    ) -> Result<Gc<ShimValue<A>>, ShimError> {
        match self {
            Self::SFn(parameters, block, captured_env) => {
                if args.len() != parameters.len() {
                    interpreter.print(b"Got unexpected number of arguments\n");
                    return Err(ShimError::Other(b"incorrect arity"));
                }

                let mut fn_env =
                    Environment::new_with_prev(captured_env.clone(), interpreter.allocator);

                for (name, value) in parameters.iter().zip(args.iter()) {
                    let name: AVec<u8, A> = name.aclone()?;
                    // TODO: don't clone this and use into_iter or something
                    fn_env.declare(name, value.aclone()?)?;
                }

                let mut fn_env = interpreter.collector.manage(ShimValue::Env(fn_env));

                std::mem::swap(&mut fn_env, &mut interpreter.env);

                let exit_result = interpreter.interpret_block(&block);
                std::mem::swap(&mut fn_env, &mut interpreter.env);
                let exit_result = exit_result?;

                match exit_result {
                    BlockExit::Finish => Ok(interpreter.g.the_unit.clone()),
                    BlockExit::Break => Ok(interpreter.g.the_unit.clone()),
                    BlockExit::Continue => Ok(interpreter.g.the_unit.clone()),
                    BlockExit::Return(val) => Ok(val),
                }
            }
            Self::BoundFn(obj, the_fn) => {
                let mut args_with_obj = AVec::new(interpreter.allocator);
                args_with_obj.push(obj.clone())?;
                for arg in args.iter() {
                    args_with_obj.push(arg.clone())?;
                }
                the_fn.borrow().call(args_with_obj, interpreter)
            }
            Self::NativeFn(boxed_fn) => boxed_fn(args, interpreter),
            _ => Err(ShimError::Other(b"value not callable")),
        }
    }

    fn block_call(
        obj: Gc<Self>,
        block: &Block<A>,
        interpreter: &mut Interpreter<A>,
    ) -> Result<Gc<ShimValue<A>>, ShimError> {
        let members = match &*obj.borrow() {
            Self::StructDef(def) => {
                // TODO: ensure we call exit_block before exiting with `?`
                interpreter.enter_block()?;

                // Create block-local variables for each member of the struct
                let mut members = AVec::new(interpreter.allocator);
                for entry in def.props.iter() {
                    if let InstanceProp::Slot(_) = entry.value() {
                        let key = entry.key().aclone()?;
                        let default = interpreter.g.the_unit.clone();
                        interpreter.env_declare(key, default)?;
                        members.push(interpreter.g.the_unit.clone())?;
                    }
                }
                // Return value is ignored, we should probably error here in
                // the parser (since a return could be ambiguous).
                interpreter.interpret_block_inner(block)?;

                // Collect the assigned values into the correct slot for the struct
                for entry in def.props.iter() {
                    if let InstanceProp::Slot(slot_num) = entry.value() {
                        // We just declared this above... why would it not be here?
                        members[*slot_num] = interpreter.env_find(entry.key()).unwrap();
                    }
                }
                interpreter.exit_block();

                members
            }
            _ => return Err(ShimError::Other(b"value not block-callable")),
        };
        interpreter.new_value(ShimValue::Struct(Struct(obj, members)))
    }

    fn get_prop(
        obj: Gc<Self>,
        name: &AVec<u8, A>,
        interpreter: &mut Interpreter<A>,
    ) -> Result<Gc<ShimValue<A>>, ShimError> {
        match (&*obj.borrow(), name.deref()) {
            (ShimValue::SString(_), b"lower") => {
                let the_fn =
                    interpreter.new_value(ShimValue::NativeFn(Box::new(|args, interpreter| {
                        assert!(args.len() == 1);
                        match &*args[0].borrow() {
                            ShimValue::SString(s) => {
                                let mut lowered = AVec::new(interpreter.allocator);
                                for c in s.iter() {
                                    lowered.push(c.to_ascii_lowercase())?;
                                }

                                interpreter.new_value(ShimValue::SString(lowered))
                            }
                            _ => Err(ShimError::Other(b"lower only str")),
                        }
                    })))?;
                interpreter.new_value(ShimValue::BoundFn(obj.clone(), the_fn))
            }
            (ShimValue::Struct(Struct(cls, slots)), _) => {
                let cls = cls.borrow();
                let prop = cls
                    .as_struct_def()
                    .ok_or(ShimError::Other(b"struct def of a struct... isn't..?"))?
                    .instance_prop(name)
                    .ok_or(ShimError::Other(b"struct does not have prop"))?;

                Ok(match prop {
                    InstanceProp::Slot(num) => slots[*num].clone(),
                    InstanceProp::Method(method) => {
                        interpreter.new_value(ShimValue::BoundFn(obj.clone(), method.clone()))?
                    }
                })
            }
            _ => Err(ShimError::Other(b"value no get_prop")),
        }
    }
}

impl<A: Allocator> Manage for ShimValue<A> {
    fn trace<'a>(&'a self, _: &mut Vec<&'a Gc<Self>>) {
        match self {
            // Immutable values don't contain Gc's
            Self::I128(_) => {}
            Self::F64(_) => {}
            Self::Bool(_) => {}
            Self::Freed => {}
            Self::Unit => {}
            Self::SString(_) => {}
            // TODO: trace these
            Self::SFn(..) => {}
            Self::BoundFn(..) => {}
            // TODO: It seems like we need to be careful not to leak GC's here
            Self::NativeFn(..) => {}
            Self::Env(..) => {}
            Self::StructDef(..) => {}
            Self::Struct(..) => {}
        }
    }

    fn cycle_break(&mut self) {
        *self = ShimValue::Freed;
    }
}

#[derive(Debug)]
pub struct Environment<A: Allocator> {
    /// `assign` and `find` may panic if there's an outstanding borrow to this Rc
    prev: Option<Gc<ShimValue<A>>>,
    map: AHashMap<AVec<u8, A>, Gc<ShimValue<A>>, A>,
}

impl<A: Allocator> Environment<A> {
    fn new(allocator: A) -> Self {
        Environment {
            prev: None,
            map: AHashMap::new(allocator),
        }
    }

    fn new_with_prev(prev: Gc<ShimValue<A>>, allocator: A) -> Self {
        Environment {
            prev: Some(prev),
            map: AHashMap::new(allocator),
        }
    }

    /// May panic if there's an outstanding borrow of `prev`
    fn assign(&mut self, name: &AVec<u8, A>, val: Gc<ShimValue<A>>) -> Result<(), ()> {
        // TODO: for values captured by closures this needs to be different.
        // Imagine an pair of closure `get_count` and `inc_count`. Incrementing
        // needs to mutate the reference shared with get_count, it can't merely
        // assign a new reference to the count when incrementing.
        if let Some(env_val) = self.map.get_mut(name) {
            Ok(*env_val = val)
        } else if let Some(prev) = &mut self.prev {
            match &mut *prev.borrow_mut() {
                ShimValue::Env(prev) => prev.assign(name, val),
                _ => Err(()),
            }
        } else {
            // TODO: better error type
            Err(())
        }
    }

    fn declare(&mut self, name: AVec<u8, A>, val: Gc<ShimValue<A>>) -> Result<(), ShimError> {
        self.map.insert(name, val)?;
        Ok(())
    }

    /// May panic if there's an outstanding borrow of `prev`
    fn find(&self, name: &AVec<u8, A>) -> Option<Gc<ShimValue<A>>> {
        if let Some(val) = self.map.get(name) {
            // NOTE: this clone does not allocate and can't fail (even when it
            // eventually moves to using ARc)
            Some(val.clone())
        } else if let Some(prev) = &self.prev {
            match &*prev.borrow() {
                ShimValue::Env(prev) => prev.find(name),
                _ => None,
            }
        } else {
            None
        }
    }
}

struct Singletons<A: Allocator> {
    the_unit: Gc<ShimValue<A>>,
}

impl<A: Allocator> Singletons<A> {
    // TODO: this should be alloc-fallible
    fn new(collector: &mut Collector<ShimValue<A>>) -> Self {
        Singletons {
            the_unit: collector.manage(ShimValue::Unit),
        }
    }
}

pub struct Interpreter<'a, A: Allocator> {
    allocator: A,
    collector: Collector<ShimValue<A>>,
    env: Gc<ShimValue<A>>,
    // TODO: figure out how to make the ABox work like this
    print: Option<&'a mut dyn Printer>,
    g: Singletons<A>,
}

pub trait Printer {
    fn print(&mut self, text: &[u8]);
}

trait NewValue<T, A: Allocator> {
    fn new_value(&mut self, val: T) -> Result<Gc<ShimValue<A>>, ShimError>;
}

impl<'a, A: Allocator> NewValue<bool, A> for Interpreter<'a, A> {
    fn new_value(&mut self, val: bool) -> Result<Gc<ShimValue<A>>, ShimError> {
        Ok(self.collector.manage(ShimValue::Bool(val)))
    }
}

impl<'a, A: Allocator> NewValue<i128, A> for Interpreter<'a, A> {
    fn new_value(&mut self, val: i128) -> Result<Gc<ShimValue<A>>, ShimError> {
        Ok(self.collector.manage(ShimValue::I128(val)))
    }
}

impl<'a, A: Allocator> NewValue<f64, A> for Interpreter<'a, A> {
    fn new_value(&mut self, val: f64) -> Result<Gc<ShimValue<A>>, ShimError> {
        Ok(self.collector.manage(ShimValue::F64(val)))
    }
}

impl<'a, A: Allocator> NewValue<ShimValue<A>, A> for Interpreter<'a, A> {
    fn new_value(&mut self, val: ShimValue<A>) -> Result<Gc<ShimValue<A>>, ShimError> {
        Ok(self.collector.manage(val))
    }
}

impl<'a, A: Allocator> NewValue<FnDef<A>, A> for Interpreter<'a, A> {
    fn new_value(&mut self, def: FnDef<A>) -> Result<Gc<ShimValue<A>>, ShimError> {
        let FnDef { args, block, .. } = def;

        Ok(self.new_value(ShimValue::SFn(args, block, self.env.clone()))?)
    }
}

enum BlockExit<A: Allocator> {
    Break,
    Continue,
    Finish,
    Return(Gc<ShimValue<A>>),
}

impl<'a, A: Allocator> Interpreter<'a, A> {
    // TODO: this should be alloc-fallible
    pub fn new(allocator: A) -> Interpreter<'a, A> {
        let mut collector = Collector::new();
        let env = collector.manage(ShimValue::Env(Environment::new(allocator)));
        let singletons = Singletons::new(&mut collector);
        Interpreter {
            allocator,
            collector: collector,
            env: env,
            print: None,
            g: singletons,
        }
    }

    fn env_map_mut<F, U>(&mut self, f: F) -> U
    where
        F: FnOnce(&mut Environment<A>) -> U,
    {
        // We're calling borrow_mut on an environment. We don't expect the
        // passed `f` to borrow it as well (since it's just assigning, mutating
        // or declaring variables).
        if let ShimValue::Env(env) = &mut *self.env.borrow_mut() {
            f(env)
        } else {
            panic!("Interpreter env is not an Environment!")
        }
    }

    fn env_map<F, U>(&self, f: F) -> U
    where
        F: FnOnce(&Environment<A>) -> U,
    {
        if let ShimValue::Env(env) = &*self.env.borrow() {
            f(env)
        } else {
            panic!("Interpreter env is not an Environment!")
        }
    }

    fn env_declare(&mut self, key: AVec<u8, A>, value: Gc<ShimValue<A>>) -> Result<(), ShimError> {
        self.env_map_mut(|env| env.declare(key, value))
    }

    fn env_assign(&mut self, key: &AVec<u8, A>, value: Gc<ShimValue<A>>) -> Result<(), ()> {
        self.env_map_mut(|env| env.assign(key, value))
    }

    fn env_find(&mut self, key: &AVec<u8, A>) -> Option<Gc<ShimValue<A>>> {
        self.env_map(|env| env.find(key))
    }

    pub fn set_print_fn(&mut self, f: &'a mut dyn Printer) {
        self.print = Some(f);
    }

    pub fn print(&mut self, text: &[u8]) {
        self.print.as_mut().map(|p| p.print(text));
    }

    pub fn interpret_expression(
        &mut self,
        expr: &Expression<A>,
    ) -> Result<Gc<ShimValue<A>>, ShimError> {
        Ok(match expr {
            Expression::Identifier(ident) => {
                // TODO: this is very dumb. AHashMap should be updated so that
                // the get method does deref-magic.
                let mut vec = AVec::new(self.allocator);
                vec.extend_from_slice(ident)?;
                let maybe_id = self.env_find(&vec);
                if let Some(id) = maybe_id {
                    id.clone()
                } else {
                    std::mem::drop(maybe_id);
                    self.print(b"Could not find ");
                    self.print(&vec);
                    self.print(b" in current environment\n");
                    return Err(ShimError::Other(b"ident not found"));
                }
            }
            Expression::IntLiteral(i) => self.new_value(*i)?,
            Expression::FloatLiteral(f) => self.new_value(*f)?,
            Expression::BoolLiteral(b) => self.new_value(*b)?,
            Expression::StringLiteral(s) => {
                let mut new_str = AVec::new(self.allocator);
                new_str.extend_from_slice(s)?;
                self.new_value(ShimValue::SString(new_str))?
            }
            Expression::Binary(op, left, right) => {
                let left = self.interpret_expression(&*left)?;
                let right = self.interpret_expression(&*right)?;

                let result = match (&*left.borrow(), &*right.borrow(), op) {
                    (ShimValue::I128(a), ShimValue::I128(b), _) => match op {
                        BinaryOp::Add => ShimValue::I128(a + b),
                        BinaryOp::Sub => ShimValue::I128(a - b),
                        BinaryOp::Mul => ShimValue::I128(a * b),
                        BinaryOp::Div => ShimValue::I128(a / b),
                        BinaryOp::Eq => ShimValue::Bool(a == b),
                        BinaryOp::Neq => ShimValue::Bool(a != b),
                        BinaryOp::Gt => ShimValue::Bool(a > b),
                        BinaryOp::Gte => ShimValue::Bool(a >= b),
                        BinaryOp::Lt => ShimValue::Bool(a < b),
                        BinaryOp::Lte => ShimValue::Bool(a <= b),
                    },
                    (ShimValue::SString(a), ShimValue::SString(b), BinaryOp::Eq) => {
                        ShimValue::Bool(a == b)
                    }
                    (ShimValue::SString(a), ShimValue::SString(b), BinaryOp::Neq) => {
                        ShimValue::Bool(a != b)
                    }
                    _ => {
                        self.print(b"TODO: values can't be bin-opped\n");
                        ShimValue::I128(42)
                    }
                };

                self.new_value(result)?
            }
            Expression::Call(cexpr) => {
                let func = self.interpret_expression(&cexpr.func)?;

                let mut args = AVec::new(self.allocator);
                for expr in cexpr.args.iter() {
                    args.push(self.interpret_expression(expr)?)?;
                }
                let x = func.borrow().call(args, self)?;
                x
            }
            Expression::BlockCall(expr, block) => {
                let obj = self.interpret_expression(expr)?;
                let x = ShimValue::block_call(obj, block, self)?;
                x
            }
            Expression::Get(gexpr, prop) => {
                let obj = self.interpret_expression(&gexpr)?;

                ShimValue::get_prop(obj, prop, self)?
            }
        })
    }

    fn interpret_block_inner(&mut self, block: &Block<A>) -> Result<BlockExit<A>, ShimError> {
        for stmt in block.stmts.iter() {
            match self.interpret_statement(stmt)? {
                Some(BlockExit::Finish) => {}
                None => {}

                // Special exit cases
                Some(BlockExit::Break) => return Ok(BlockExit::Break),
                Some(BlockExit::Continue) => return Ok(BlockExit::Continue),
                Some(BlockExit::Return(val)) => return Ok(BlockExit::Return(val)),
            }
        }

        // TODO: return a ShimValue if the last expression didn't end with a
        // semicolon (like Rust)
        Ok(BlockExit::Finish)
    }

    fn enter_block(&mut self) -> Result<(), AllocError> {
        let mut new_env = self
            .collector
            .manage(ShimValue::Env(Environment::new(self.allocator)));

        // Assign new_env to .env
        std::mem::swap(&mut self.env, &mut new_env);

        // Assign the old env to .prev (since it's now in new_env)
        self.env_map_mut(|env| env.prev = Some(new_env));

        Ok(())
    }

    fn exit_block(&mut self) {
        let mut prev_env = self.env_map(|env| env.prev.as_ref().unwrap().clone());

        std::mem::swap(&mut self.env, &mut prev_env);
    }

    fn interpret_block(&mut self, block: &Block<A>) -> Result<BlockExit<A>, ShimError> {
        Self::enter_block(self)?;
        let res = self.interpret_block_inner(block);
        Self::exit_block(self);

        res
    }

    fn interpret_statement(
        &mut self,
        stmt: &Statement<A>,
    ) -> Result<Option<BlockExit<A>>, ShimError> {
        match stmt {
            Statement::Expression(expr) => {
                self.interpret_expression(expr)?;
            }
            Statement::Declaration(name, expr) => {
                let id = self.interpret_expression(expr)?;
                let name_clone: AVec<u8, A> = name.aclone()?;
                self.env_declare(name_clone, id)?;
            }
            Statement::Assignment(name, expr) => {
                // TODO: this should write the value to the existing id rather
                // than writing a new id.
                let val = self.interpret_expression(expr)?;
                let assign_result = self.env_assign(name, val);
                if assign_result.is_err() {
                    self.print(b"Variable ");
                    self.print(&name);
                    self.print(b" has not been declared\n");
                    return Err(ShimError::Other(b"ident not found"));
                }
            }
            Statement::Block(block) => {
                self.interpret_block(block)?;
            }
            Statement::IfStatement(if_stmt) => {
                let predicate_result = self.interpret_expression(&if_stmt.predicate)?;
                let exit_result = if predicate_result.borrow().is_truthy() {
                    self.interpret_block(&if_stmt.if_block)?
                } else if let Some(else_block) = &if_stmt.else_block {
                    self.interpret_block(else_block)?
                } else {
                    BlockExit::Finish
                };

                match exit_result {
                    BlockExit::Break => return Ok(Some(BlockExit::Break)),
                    BlockExit::Continue => return Ok(Some(BlockExit::Continue)),
                    BlockExit::Finish => {}
                    BlockExit::Return(expr) => return Ok(Some(BlockExit::Return(expr))),
                }
            }
            Statement::WhileStatement(predicate, block) => loop {
                let predicate = self.interpret_expression(&predicate)?;
                if !predicate.borrow().is_truthy() {
                    break;
                }
                match self.interpret_block(&block)? {
                    BlockExit::Break => return Ok(Some(BlockExit::Finish)),
                    BlockExit::Continue | BlockExit::Finish => continue,
                    BlockExit::Return(expr) => return Ok(Some(BlockExit::Return(expr))),
                }
            },
            Statement::BreakStatement => return Ok(Some(BlockExit::Break)),
            Statement::ContinueStatement => return Ok(Some(BlockExit::Continue)),
            Statement::ReturnStatement(expr) => {
                let val = self.interpret_expression(expr)?;
                return Ok(Some(BlockExit::Return(val)));
            }
            Statement::FnDef(def) => {
                let name = def.name.aclone()?;
                // TODO: a significant amount of effort would be saved if this
                // block didn't need to be cloned since it forces a _bunch_ of
                // other types to need to be cloned as well.
                // Maybe it should be an Rc block?
                let fn_obj = self.new_value(def.aclone()?)?;
                self.env_declare(name, fn_obj)?;
            }
            Statement::StructDef(name, members, methods) => {
                let mut props: AHashMap<AVec<u8, A>, InstanceProp<A>, A> =
                    AHashMap::new(self.allocator);
                for (slot, member) in members.iter().enumerate() {
                    props.insert(member.aclone()?, InstanceProp::Slot(slot))?;
                }

                for method in methods.iter() {
                    props.insert(
                        method.name.aclone()?,
                        InstanceProp::Method(self.new_value(method.aclone()?)?),
                    )?;
                }

                let struct_def = self.new_value(ShimValue::StructDef(StructDef { props }))?;
                self.env_declare(name.aclone()?, struct_def)?;
            }
        }

        Ok(None)
    }

    pub fn interpret(&mut self, text: &'a [u8]) -> Result<(), ShimError> {
        // TODO: make `new` fallible and put this there
        let mut print_name: AVec<u8, A> = AVec::new(self.allocator);
        print_name.extend_from_slice(b"print")?;
        let print_fn = self
            .collector
            .manage(ShimValue::NativeFn(Box::new(|args, interpreter| {
                let last_idx = args.len() as isize - 1;
                for (idx, arg) in args.iter().enumerate() {
                    let arg_str: AVec<u8, A> = arg.borrow().stringify(interpreter.allocator)?;
                    interpreter.print(&arg_str);

                    if idx as isize != last_idx {
                        interpreter.print(b" ");
                    }
                }
                interpreter.print(b"\n");
                Ok(interpreter.g.the_unit.clone())
            })));
        self.env_declare(print_name, print_fn)?;

        let mut tokens = TokenStream::new(text);
        let script = match parse_script(&mut tokens, self.allocator) {
            Ok(script) => script,
            Err(ParseError::PureParseError(PureParseError::Generic(msg))) => {
                self.print(msg);
                self.print(b"\n");
                return Err(ParseError::PureParseError(PureParseError::Generic(msg)).into());
            }
            Err(err) => {
                self.print(b"Error parsing input");
                return Err(err.into());
            }
        };

        match self.interpret_block(&script) {
            Ok(_) => {}
            Err(ShimError::Other(msg)) => {
                self.print(b"ERROR: ");
                self.print(msg);
                self.print(b"\n");
            }
            Err(_) => {
                self.print(b"ERROR: Misc error\n");
            }
        }

        // TODO: figure out where to put this...
        self.collector.collect_cycles();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn print_int() {
        let text = b"print(3);";
        let mut tokens = TokenStream::new(text);
        assert!(tokens.peek() == Token::Identifier(b"print"));
        tokens.advance();
        assert!(tokens.matches(Token::LeftParen));
        assert!(tokens.peek() == Token::IntLiteral(3));
        tokens.advance();
        assert!(tokens.matches(Token::RightParen));
        assert!(tokens.matches(Token::Semicolon));
        assert!(tokens.matches(Token::EOF));

        // Make sure that `matches` doesn't always return true :)
        assert!(!tokens.matches(Token::DoubleStar));
    }

    #[test]
    fn tokenize_float() {
        let text = b"1.1";
        let mut tokens = TokenStream::new(text);

        assert_eq!(tokens.peek(), Token::FloatLiteral(1.1));
    }

    #[test]
    fn tokenize_with_newline() {
        let text = b"print(0);\nprint(0);";
        let mut tokens = TokenStream::new(text);

        for _ in 1..=2 {
            assert!(tokens.peek() == Token::Identifier(b"print"));
            tokens.advance();
            assert!(tokens.matches(Token::LeftParen));
            assert!(tokens.peek() == Token::IntLiteral(0));
            tokens.advance();
            assert!(tokens.matches(Token::RightParen));
            assert!(tokens.matches(Token::Semicolon));
        }

        assert!(tokens.matches(Token::EOF));
    }

    #[test]
    fn tokenize_if() {
        let text = b"if";
        let mut tokens = TokenStream::new(text);

        assert!(tokens.peek() == Token::IfKeyword);
    }

    #[test]
    fn tokenize_str() {
        let text = br#"print("hi");"#;
        let mut tokens = TokenStream::new(text);

        assert!(tokens.peek() == Token::Identifier(b"print"));
        tokens.advance();
        assert!(tokens.matches(Token::LeftParen));
        assert!(tokens.peek() == Token::StringLiteral(b"hi"));
        tokens.advance();
        assert!(tokens.matches(Token::RightParen));
        assert!(tokens.matches(Token::Semicolon));
    }

    #[test]
    fn tokenize_str2() {
        let text = br#"
            let a = "foo";
            let b = a;
            print(b);
        "#;
        let mut tokens = TokenStream::new(text);

        assert!(tokens.matches(Token::LetKeyword));
        assert!(tokens.peek() == Token::Identifier(b"a"));
        tokens.advance();
        assert!(tokens.matches(Token::Equal));
        assert!(tokens.peek() == Token::StringLiteral(b"foo"));
        tokens.advance();
        assert!(tokens.matches(Token::Semicolon));

        assert!(tokens.matches(Token::LetKeyword));
        assert!(tokens.peek() == Token::Identifier(b"b"));
        tokens.advance();
        assert!(tokens.matches(Token::Equal));
        assert!(tokens.peek() == Token::Identifier(b"a"));
        tokens.advance();
        assert!(tokens.matches(Token::Semicolon));

        assert!(tokens.peek() == Token::Identifier(b"print"));
        tokens.advance();
        assert!(tokens.matches(Token::LeftParen));
        assert!(tokens.peek() == Token::Identifier(b"b"));
        tokens.advance();
        assert!(tokens.matches(Token::RightParen));
        assert!(tokens.matches(Token::Semicolon));
    }
}
