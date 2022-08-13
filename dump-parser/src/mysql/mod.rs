use std::fmt;
use std::iter::Peekable;
use std::str::Chars;

use crate::mysql::Keyword::{
    Add, Alter, Constraint, Copy, Create, Database, Foreign, From, Insert, Into as KeywordInto,
    Key, NoKeyword, Not, Null, Primary, References, Table,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Token {
    /// An end-of-file marker, not a real token
    EOF,
    /// A signed numeric literal
    Number(String, bool),
    /// TABLE instruction
    Word(Word),
    /// Whitespace (space, tab, etc)
    Whitespace(Whitespace),
    /// A character that could not be tokenized
    Char(char),
    /// Single quoted string: i.e: 'string'
    SingleQuotedString(String),
    /// "National" string literal: i.e: N'string'
    NationalStringLiteral(String),
    /// Hexadecimal string literal: i.e.: X'deadbeef'
    HexStringLiteral(String),
    /// Comma
    Comma,
    /// Double equals sign `==`
    DoubleEq,
    /// Equality operator `=`
    Eq,
    /// Not Equals operator `<>` (or `!=` in some dialects)
    Neq,
    /// Less Than operator `<`
    Lt,
    /// Greater Than operator `>`
    Gt,
    /// Less Than Or Equals operator `<=`
    LtEq,
    /// Greater Than Or Equals operator `>=`
    GtEq,
    /// Spaceship operator <=>
    Spaceship,
    /// Plus operator `+`
    Plus,
    /// Minus operator `-`
    Minus,
    /// Multiplication operator `*`
    Mul,
    /// Division operator `/`
    Div,
    /// Modulo Operator `%`
    Mod,
    /// String concatenation `||`
    StringConcat,
    /// Left parenthesis `(`
    LParen,
    /// Right parenthesis `)`
    RParen,
    /// Period (used for compound identifiers or projections into nested types)
    Period,
    /// Colon `:`
    Colon,
    /// DoubleColon `::` (used for casting in postgresql)
    DoubleColon,
    /// SemiColon `;` used as separator for COPY and payload
    SemiColon,
    /// Backslash `\` used in terminating the COPY payload with `\.`
    Backslash,
    /// Left bracket `[`
    LBracket,
    /// Right bracket `]`
    RBracket,
    /// Ampersand `&`
    Ampersand,
    /// Pipe `|`
    Pipe,
    /// Caret `^`
    Caret,
    /// Left brace `{`
    LBrace,
    /// Right brace `}`
    RBrace,
    /// Right Arrow `=>`
    RArrow,
    /// Sharp `#` used for PostgreSQL Bitwise XOR operator
    Sharp,
    /// Tilde `~` used for PostgreSQL Bitwise NOT operator or case sensitive match regular expression operator
    Tilde,
    /// `~*` , a case insensitive match regular expression operator in PostgreSQL
    TildeAsterisk,
    /// `!~` , a case sensitive not match regular expression operator in PostgreSQL
    ExclamationMarkTilde,
    /// `!~*` , a case insensitive not match regular expression operator in PostgreSQL
    ExclamationMarkTildeAsterisk,
    /// `<<`, a bitwise shift left operator in PostgreSQL
    ShiftLeft,
    /// `>>`, a bitwise shift right operator in PostgreSQL
    ShiftRight,
    /// Exclamation Mark `!` used for PostgreSQL factorial operator
    ExclamationMark,
    /// Double Exclamation Mark `!!` used for PostgreSQL prefix factorial operator
    DoubleExclamationMark,
    /// AtSign `@` used for PostgreSQL abs operator
    AtSign,
    /// `?` or `$` , a prepared statement arg placeholder
    Placeholder(String),
}

impl Token {
    pub fn make_keyword(keyword: &str) -> Self {
        Token::make_word(keyword, None)
    }

    pub fn make_word(word: &str, quote_style: Option<char>) -> Self {
        let word_uppercase = word.to_uppercase();
        Token::Word(Word {
            value: word.to_string(),
            quote_style,
            keyword: if quote_style == None {
                match word_uppercase.as_str() {
                    "ALTER" => Alter,
                    "CREATE" => Create,
                    "INSERT" => Insert,
                    "INTO" => KeywordInto,
                    "COPY" => Copy,
                    "DATABASE" => Database,
                    "TABLE" => Table,
                    "FROM" => From,
                    "NOT" => Not,
                    "NULL" => Null,
                    "ADD" => Add,
                    "CONSTRAINT" => Constraint,
                    "PRIMARY" => Primary,
                    "FOREIGN" => Foreign,
                    "REFERENCES" => References,
                    "KEY" => Key,
                    _ => NoKeyword,
                }
            } else {
                Keyword::NoKeyword
            },
        })
    }
}

/// A keyword (like SELECT) or an optionally quoted SQL identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Word {
    /// The value of the token, without the enclosing quotes, and with the
    /// escape sequences (if any) processed.
    /// TODO: escapes are not handled
    pub value: String,
    /// An identifier can be "quoted" (&lt;delimited identifier> in ANSI parlance).
    /// The standard and most implementations allow using double quotes for this,
    /// but some implementations support other quoting styles as well (e.g. \[MS SQL])
    pub quote_style: Option<char>,
    /// If the word was not quoted and it matched one of the known keywords,
    /// this will have one of the values from dialect::keywords, otherwise empty
    pub keyword: Keyword,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Keyword {
    Create,
    Alter,
    Insert,
    Into,
    Copy,
    Database,
    Table,
    From,
    Not,
    Null,
    Add,
    Constraint,
    Primary,
    Foreign,
    References,
    Key,
    NoKeyword,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Whitespace {
    Space,
    Newline,
    Tab,
    SingleLineComment { comment: String, prefix: String },
    MultiLineComment(String),
}

/// Tokenizer error
#[derive(Debug, PartialEq)]
pub struct TokenizerError {
    pub message: String,
    pub line: u64,
    pub col: u64,
}

impl fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at Line: {}, Column {}",
            self.message, self.line, self.col
        )
    }
}

/// SQL Tokenizer
pub struct Tokenizer<'a> {
    query: &'a str,
    line: u64,
    col: u64,
}

impl<'a> Tokenizer<'a> {
    /// Create a new DUMP SQL tokenizer for the specified DUMP SQL statement
    pub fn new<S: Into<&'a str>>(query: S) -> Self {
        Self {
            query: query.into(),
            line: 1,
            col: 1,
        }
    }

    /// Tokenize the statement and produce a vector of tokens
    pub fn tokenize(&mut self) -> Result<Vec<Token>, TokenizerError> {
        let mut peekable = self.query.chars().peekable();

        let mut tokens: Vec<Token> = vec![];

        while let Some(token) = self.next_token(&mut peekable)? {
            match &token {
                Token::Whitespace(Whitespace::Newline) => {
                    self.line += 1;
                    self.col = 1;
                }

                Token::Whitespace(Whitespace::Tab) => self.col += 4,
                _ => self.col += 1,
            }

            tokens.push(token);
        }

        Ok(tokens)
    }

    /// Get the next token or return None
    fn next_token(&self, chars: &mut Peekable<Chars<'_>>) -> Result<Option<Token>, TokenizerError> {
        //println!("next_token: {:?}", chars.peek());
        match chars.peek() {
            Some(&ch) => match ch {
                ' ' => self.consume_and_return(chars, Token::Whitespace(Whitespace::Space)),
                '\t' => self.consume_and_return(chars, Token::Whitespace(Whitespace::Tab)),
                '\n' => self.consume_and_return(chars, Token::Whitespace(Whitespace::Newline)),
                '\r' => {
                    // Emit a single Whitespace::Newline token for \r and \r\n
                    chars.next();
                    if let Some('\n') = chars.peek() {
                        chars.next();
                    }
                    Ok(Some(Token::Whitespace(Whitespace::Newline)))
                }
                'N' => {
                    chars.next(); // consume, to check the next char
                    match chars.peek() {
                        Some('\'') => {
                            // N'...' - a <national character string literal>
                            let s = self.tokenize_single_quoted_string(chars)?;
                            Ok(Some(Token::NationalStringLiteral(s)))
                        }
                        _ => {
                            // regular identifier starting with an "N"
                            let s = self.tokenize_word('N', chars);
                            Ok(Some(Token::make_word(&s, None)))
                        }
                    }
                }
                // The spec only allows an uppercase 'X' to introduce a hex
                // string, but PostgreSQL, at least, allows a lowercase 'x' too.
                x @ 'x' | x @ 'X' => {
                    chars.next(); // consume, to check the next char
                    match chars.peek() {
                        Some('\'') => {
                            // X'...' - a <binary string literal>
                            let s = self.tokenize_single_quoted_string(chars)?;
                            Ok(Some(Token::HexStringLiteral(s)))
                        }
                        _ => {
                            // regular identifier starting with an "X"
                            let s = self.tokenize_word(x, chars);
                            Ok(Some(Token::make_word(&s, None)))
                        }
                    }
                }
                // identifier or keyword
                ch if is_identifier_start(ch) => {
                    chars.next(); // consume the first char
                    let s = self.tokenize_word(ch, chars);

                    if s.chars().all(|x| ('0'..='9').contains(&x) || x == '.') {
                        let mut s = peeking_take_while(&mut s.chars().peekable(), |ch| {
                            matches!(ch, '0'..='9' | '.')
                        });
                        let s2 = peeking_take_while(chars, |ch| matches!(ch, '0'..='9' | '.'));
                        s += s2.as_str();
                        return Ok(Some(Token::Number(s, false)));
                    }
                    Ok(Some(Token::make_word(&s, None)))
                }
                // string
                '\'' | '`' => {
                    let s = self.tokenize_single_quoted_string(chars)?;

                    Ok(Some(Token::SingleQuotedString(s)))
                }
                // numbers and period
                '0'..='9' | '.' => self.tokenize_number_literal(chars, None),
                // punctuation
                '(' => self.consume_and_return(chars, Token::LParen),
                ')' => self.consume_and_return(chars, Token::RParen),
                ',' => self.consume_and_return(chars, Token::Comma),
                // operators
                '-' => {
                    chars.next(); // consume the '-'
                    match chars.peek() {
                        Some('-') => {
                            chars.next(); // consume the second '-', starting a single-line comment
                            let comment = self.tokenize_single_line_comment(chars);
                            Ok(Some(Token::Whitespace(Whitespace::SingleLineComment {
                                prefix: "--".to_owned(),
                                comment,
                            })))
                        }
                        // This is still not exhaustive as "SELECT - 1 as test" in postgres would return a numeric -1.
                        Some('0'..='9') => self.tokenize_number_literal(chars, Some('-')),
                        // a regular '-' operator
                        _ => Ok(Some(Token::Minus)),
                    }
                }
                '/' => {
                    chars.next(); // consume the '/'
                    match chars.peek() {
                        Some('*') => {
                            chars.next(); // consume the '*', starting a multi-line comment
                            self.tokenize_multiline_comment(chars)
                        }
                        // a regular '/' operator
                        _ => Ok(Some(Token::Div)),
                    }
                }
                '+' => {
                    chars.next(); // consume the '+'
                    match chars.peek() {
                        // This is still not exhaustive as "SELECT + 1 as test" in postgres would return a numeric 1.
                        Some('0'..='9') => self.tokenize_number_literal(chars, Some('+')),
                        // a regular '-' operator
                        _ => Ok(Some(Token::Plus)),
                    }
                }
                '*' => self.consume_and_return(chars, Token::Mul),
                '%' => self.consume_and_return(chars, Token::Mod),
                '=' => {
                    chars.next(); // consume
                    match chars.peek() {
                        Some('>') => self.consume_and_return(chars, Token::RArrow),
                        _ => Ok(Some(Token::Eq)),
                    }
                }
                '!' => {
                    chars.next(); // consume
                    match chars.peek() {
                        Some('=') => self.consume_and_return(chars, Token::Neq),
                        Some('!') => self.consume_and_return(chars, Token::DoubleExclamationMark),
                        Some('~') => {
                            chars.next();
                            match chars.peek() {
                                Some('*') => self
                                    .consume_and_return(chars, Token::ExclamationMarkTildeAsterisk),
                                _ => Ok(Some(Token::ExclamationMarkTilde)),
                            }
                        }
                        _ => Ok(Some(Token::ExclamationMark)),
                    }
                }
                '<' => {
                    chars.next(); // consume
                    match chars.peek() {
                        Some('=') => {
                            chars.next();
                            match chars.peek() {
                                Some('>') => self.consume_and_return(chars, Token::Spaceship),
                                _ => Ok(Some(Token::LtEq)),
                            }
                        }
                        Some('>') => self.consume_and_return(chars, Token::Neq),
                        Some('<') => self.consume_and_return(chars, Token::ShiftLeft),
                        _ => Ok(Some(Token::Lt)),
                    }
                }
                '>' => {
                    chars.next(); // consume
                    match chars.peek() {
                        Some('=') => self.consume_and_return(chars, Token::GtEq),
                        Some('>') => self.consume_and_return(chars, Token::ShiftRight),
                        _ => Ok(Some(Token::Gt)),
                    }
                }
                ':' => {
                    chars.next();
                    match chars.peek() {
                        Some(':') => self.consume_and_return(chars, Token::DoubleColon),
                        _ => Ok(Some(Token::Colon)),
                    }
                }
                ';' => self.consume_and_return(chars, Token::SemiColon),
                '\\' => self.consume_and_return(chars, Token::Backslash),
                '[' => self.consume_and_return(chars, Token::LBracket),
                ']' => self.consume_and_return(chars, Token::RBracket),
                '&' => self.consume_and_return(chars, Token::Ampersand),
                '^' => self.consume_and_return(chars, Token::Caret),
                '{' => self.consume_and_return(chars, Token::LBrace),
                '}' => self.consume_and_return(chars, Token::RBrace),
                '~' => {
                    chars.next(); // consume
                    match chars.peek() {
                        Some('*') => self.consume_and_return(chars, Token::TildeAsterisk),
                        _ => Ok(Some(Token::Tilde)),
                    }
                }
                '#' => self.consume_and_return(chars, Token::Sharp),
                '@' => self.consume_and_return(chars, Token::AtSign),
                '?' => self.consume_and_return(chars, Token::Placeholder(String::from("?"))),
                '$' => {
                    chars.next();
                    let s = peeking_take_while(
                        chars,
                        |ch| matches!(ch, '0'..='9' | 'A'..='Z' | 'a'..='z'),
                    );
                    Ok(Some(Token::Placeholder(String::from("$") + &s)))
                }
                other => self.consume_and_return(chars, Token::Char(other)),
            },
            None => Ok(None),
        }
    }

    fn tokenizer_error<R>(&self, message: impl Into<String>) -> Result<R, TokenizerError> {
        Err(TokenizerError {
            message: message.into(),
            col: self.col,
            line: self.line,
        })
    }

    // Consume characters until newline
    fn tokenize_single_line_comment(&self, chars: &mut Peekable<Chars<'_>>) -> String {
        let mut comment = peeking_take_while(chars, |ch| ch != '\n');
        if let Some(ch) = chars.next() {
            assert_eq!(ch, '\n');
            comment.push(ch);
        }
        comment
    }

    /// Tokenize an identifier or keyword, after the first char is already consumed.
    fn tokenize_word(&self, first_char: char, chars: &mut Peekable<Chars<'_>>) -> String {
        let mut s = first_char.to_string();
        s.push_str(&peeking_take_while(chars, |ch| is_identifier_part(ch)));
        s
    }

    /// Read a single quoted string, starting with the opening quote.
    fn tokenize_single_quoted_string(
        &self,
        chars: &mut Peekable<Chars<'_>>,
    ) -> Result<String, TokenizerError> {
        let mut s = String::new();
        let quote_char = chars.next().expect("opening quote character"); // consume the opening quote

        // MySQL escape sequances - https://dev.mysql.com/doc/refman/5.6/en/string-literals.html#character-escape-sequences
        // slash escaping is specific to some dialect
        while let Some(&ch) = chars.peek() {
            match ch {
                '\\' => {
                    // next char is escaped
                    chars.next(); // consume
                    s.push(ch);
                    if let Some(next_char) = chars.next() {
                        s.push(next_char);
                    }
                }
                b if b == quote_char => {
                    chars.next(); // consume
                    return Ok(s);
                }
                _ => {
                    chars.next(); // consume
                    s.push(ch);
                }
            }
        }

        self.tokenizer_error("Unterminated string literal")
    }

    // Read a signed number literal
    fn tokenize_number_literal(
        &self,
        chars: &mut Peekable<Chars<'_>>,
        sign: Option<char>
    ) -> Result<Option<Token>, TokenizerError> {
        let mut s = match sign {
            Some(ch) if ch == '+' || ch == '-' => {
                String::from(ch) + &peeking_take_while(chars, |ch| matches!(ch, '0'..='9'))
            }
            Some(_) => panic!("invalid sign"),
            None => peeking_take_while(chars, |ch| matches!(ch, '0'..='9'))
        };

        // match binary literal that starts with 0x
        if s == "0" && chars.peek() == Some(&'x') {
            chars.next();
            let s2 = peeking_take_while(
                chars,
                |ch| matches!(ch, '0'..='9' | 'A'..='F' | 'a'..='f'),
            );
            return Ok(Some(Token::HexStringLiteral(s2)));
        }

        // match one period
        if let Some('.') = chars.peek() {
            s.push('.');
            chars.next();
        }
        s += &peeking_take_while(chars, |ch| matches!(ch, '0'..='9'));

        // No number -> Token::Period
        if s == "." {
            return Ok(Some(Token::Period));
        }

        let long = if chars.peek() == Some(&'L') {
            chars.next();
            true
        } else {
            false
        };
        Ok(Some(Token::Number(s, long)))
    }

    fn tokenize_multiline_comment(
        &self,
        chars: &mut Peekable<Chars<'_>>,
    ) -> Result<Option<Token>, TokenizerError> {
        let mut s = String::new();
        let mut maybe_closing_comment = false;
        // TODO: deal with nested comments
        loop {
            match chars.next() {
                Some(ch) => {
                    if maybe_closing_comment {
                        if ch == '/' {
                            break Ok(Some(Token::Whitespace(Whitespace::MultiLineComment(s))));
                        } else {
                            s.push('*');
                        }
                    }
                    maybe_closing_comment = ch == '*';
                    if !maybe_closing_comment {
                        s.push(ch);
                    }
                }
                None => break self.tokenizer_error("Unexpected EOF while in a multi-line comment"),
            }
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn consume_and_return(
        &self,
        chars: &mut Peekable<Chars<'_>>,
        t: Token,
    ) -> Result<Option<Token>, TokenizerError> {
        chars.next();
        Ok(Some(t))
    }
}

fn is_identifier_start(ch: char) -> bool {
    // See https://www.postgresql.org/docs/14/sql-syntax-lexical.html#SQL-SYNTAX-IDENTIFIERS
    // We don't yet support identifiers beginning with "letters with
    // diacritical marks and non-Latin letters"
    ('a'..='z').contains(&ch) || ('A'..='Z').contains(&ch) || ch == '_'
}

fn is_identifier_part(ch: char) -> bool {
    ('a'..='z').contains(&ch)
        || ('A'..='Z').contains(&ch)
        || ('0'..='9').contains(&ch)
        || ch == '$'
        || ch == '_'
}

/// Read from `chars` until `predicate` returns `false` or EOF is hit.
/// Return the characters read as String, and keep the first non-matching
/// char available as `chars.next()`.
fn peeking_take_while(
    chars: &mut Peekable<Chars<'_>>,
    mut predicate: impl FnMut(char) -> bool,
) -> String {
    let mut s = String::new();
    while let Some(&ch) = chars.peek() {
        if predicate(ch) {
            chars.next(); // consume
            s.push(ch);
        } else {
            break;
        }
    }

    s
}

pub fn match_keyword_at_position(keyword: Keyword, tokens: &Vec<Token>, pos: usize) -> bool {
    if let Some(token) = tokens.get(pos) {
        return match token {
            Token::Word(word) => word.keyword == keyword,
            _ => false,
        };
    };

    false
}

pub fn get_word_value_at_position(tokens: &Vec<Token>, pos: usize) -> Option<&str> {
    if let Some(fifth_token) = tokens.get(pos) {
        return match fifth_token {
            Token::Word(word) => Some(word.value.as_str()),
            _ => None,
        };
    }

    None
}

pub fn get_single_quoted_string_value_at_position(tokens: &Vec<Token>, pos: usize) -> Option<&str> {
    if let Some(token) = tokens.get(pos) {
        return match token {
            Token::SingleQuotedString(quoted_string) => Some(quoted_string.as_str()),
            _ => None,
        };
    }

    None
}

pub fn get_column_names_from_insert_into_query(tokens: &Vec<Token>) -> Vec<&str> {
    if !match_keyword_at_position(Keyword::Insert, &tokens, 0)
        || !match_keyword_at_position(Keyword::Into, &tokens, 2)
    {
        // it means that the query is not an INSERT INTO.. one
        return Vec::new();
    }

    tokens
        .iter()
        .skip_while(|token| match **token {
            Token::LParen => false,
            _ => true,
        })
        .take_while(|token| match **token {
            Token::RParen => false,
            _ => true,
        })
        .filter_map(|token| match token {
            Token::Word(word) => Some(word.value.as_str()), // column name
            Token::SingleQuotedString(name) => Some(name.as_str()), // also column name
            _ => None,
        })
        .collect::<Vec<_>>()
}

pub fn get_column_values_from_insert_into_query(tokens: &Vec<Token>) -> Vec<&Token> {
    if !match_keyword_at_position(Keyword::Insert, &tokens, 0)
        || !match_keyword_at_position(Keyword::Into, &tokens, 2)
    {
        // it means that the query is not an INSERT INTO.. one
        return Vec::new();
    }

    tokens
        .iter()
        .skip_while(|token| match **token {
            Token::RParen => false,
            _ => true,
        })
        .skip_while(|token| match **token {
            Token::LParen => false,
            _ => true,
        })
        .take_while(|token| match **token {
            Token::RParen => false,
            _ => true,
        })
        .filter_map(|token| match token {
            Token::Comma | Token::Whitespace(_) | Token::LParen | Token::RParen => None,
            token => Some(token), // column value
        })
        .collect::<Vec<_>>()
}

pub fn get_tokens_from_query_str(query: &str) -> Vec<Token> {
    // query by query
    let mut tokenizer = Tokenizer::new(query);

    let tokens = match tokenizer.tokenize() {
        Ok(tokens) => tokens,
        Err(err) => {
            println!("failing query: '{}'", query);
            panic!("{:?}", err)
        }
    };

    trim_pre_whitespaces(tokens)
}

pub fn trim_pre_whitespaces(tokens: Vec<Token>) -> Vec<Token> {
    tokens
        .into_iter()
        .skip_while(|token| match token {
            // remove whitespaces (and comments) at the beginning of a vec of tokens
            Token::Whitespace(_) => true,
            _ => false,
        })
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    use crate::mysql::{
        get_column_names_from_insert_into_query, get_column_values_from_insert_into_query,
        get_single_quoted_string_value_at_position, get_tokens_from_query_str,
        match_keyword_at_position, trim_pre_whitespaces, Token, Tokenizer, Whitespace,
    };

    #[test]
    fn test_tokenize_single_quoted_string() {
        // single quoted strings must end with a commar or a closing parenthese.
        let q = "'People\\'sRepublic',";
        let tokenizer = Tokenizer::new(q);
        let mut chars = q.chars().peekable();
        let single_quoted_string = tokenizer.tokenize_single_quoted_string(&mut chars).unwrap();
        assert_eq!(single_quoted_string, "People\\'sRepublic".to_string());

        let q = "'People\\'sRepublic')";
        let tokenizer = Tokenizer::new(q);
        let mut chars = q.chars().peekable();
        let single_quoted_string = tokenizer.tokenize_single_quoted_string(&mut chars).unwrap();
        assert_eq!(single_quoted_string, "People\\'sRepublic".to_string());
    }

    #[test]
    fn tokenizer_for_create_table_query() {
        let q = r"
CREATE TABLE `customer_store` (
  `store_id` int NOT NULL COMMENT 'Field sample comment',
  `customer_id` int NOT NULL,
  KEY `customer_store_store_id_customer_id_index` (`store_id`,`customer_id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='Table sample comment';";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert!(tokens_result.is_ok());

        let expected = vec![
            Token::Whitespace(Whitespace::Newline),
            Token::make_keyword("CREATE"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("TABLE"),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("customer_store".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::LParen,
            Token::Whitespace(Whitespace::Newline),
            Token::Whitespace(Whitespace::Space),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("store_id".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::make_word("int", None),
            Token::Whitespace(Whitespace::Space),
            Token::make_word("NOT", None),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("NULL"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("COMMENT"),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("Field sample comment".to_string()),
            Token::Comma,
            Token::Whitespace(Whitespace::Newline),
            Token::Whitespace(Whitespace::Space),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("customer_id".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::make_word("int", None),
            Token::Whitespace(Whitespace::Space),
            Token::make_word("NOT", None),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("NULL"),
            Token::Comma,
            Token::Whitespace(Whitespace::Newline),
            Token::Whitespace(Whitespace::Space),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("KEY"),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("customer_store_store_id_customer_id_index".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::LParen,
            Token::SingleQuotedString("store_id".to_string()),
            Token::Comma,
            Token::SingleQuotedString("customer_id".to_string()),
            Token::RParen,
            Token::Whitespace(Whitespace::Newline),
            Token::RParen,
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("ENGINE"),
            Token::Eq,
            Token::make_keyword("InnoDB"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("DEFAULT"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("CHARSET"),
            Token::Eq,
            Token::make_keyword("utf8mb4"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("COLLATE"),
            Token::Eq,
            Token::make_keyword("utf8mb4_unicode_ci"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("COMMENT"),
            Token::Eq,
            Token::SingleQuotedString("Table sample comment".to_string()),
            Token::SemiColon,
        ];

        assert_eq!(tokens_result.unwrap(), expected);
    }

    #[test]
    fn tokenizer_for_insert_into_query() {
        let q = "INSERT INTO `images` VALUES (1,6,640,480,'https://via.placeholder.com/640x480.png/006633?text=animals+quo',1,'2022-04-13 19:21:30','2022-04-13 19:21:30');";
        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert!(tokens_result.is_ok());

        let expected = vec![
            Token::make_keyword("INSERT"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("INTO"),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("images".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("VALUES"),
            Token::Whitespace(Whitespace::Space),
            Token::LParen,
            Token::Number("1".to_string(), false),
            Token::Comma,
            Token::Number("6".to_string(), false),
            Token::Comma,
            Token::Number("640".to_string(), false),
            Token::Comma,
            Token::Number("480".to_string(), false),
            Token::Comma,
            Token::SingleQuotedString(
                "https://via.placeholder.com/640x480.png/006633?text=animals+quo".to_string(),
            ),
            Token::Comma,
            Token::Number("1".to_string(), false),
            Token::Comma,
            Token::SingleQuotedString("2022-04-13 19:21:30".to_string()),
            Token::Comma,
            Token::SingleQuotedString("2022-04-13 19:21:30".to_string()),
            Token::RParen,
            Token::SemiColon,
        ];
        assert_eq!(tokens_result.unwrap(), expected);
    }

    #[test]
    fn tokenize_insert_into_with_special_chars() {
        let q = "INSERT INTO `country` VALUES ('CHN','China','Asia','Eastern Asia',9572900.00,-1523,+1277558000,71.4,982268.00,917719.00,'Zhongquo','People\\'sRepublic','Jiang Zemin',1891,'CN');";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert!(tokens_result.is_ok());

        let expected = vec![
            Token::make_keyword("INSERT"),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("INTO"),
            Token::Whitespace(Whitespace::Space),
            Token::SingleQuotedString("country".to_string()),
            Token::Whitespace(Whitespace::Space),
            Token::make_keyword("VALUES"),
            Token::Whitespace(Whitespace::Space),
            Token::LParen,
            Token::SingleQuotedString("CHN".to_string()),
            Token::Comma,
            Token::SingleQuotedString("China".to_string()),
            Token::Comma,
            Token::SingleQuotedString("Asia".to_string()),
            Token::Comma,
            Token::SingleQuotedString("Eastern Asia".to_string()),
            Token::Comma,
            Token::Number("9572900.00".to_string(), false),
            Token::Comma,
            Token::Number("-1523".to_string(), false),
            Token::Comma,
            Token::Number("+1277558000".to_string(), false),
            Token::Comma,
            Token::Number("71.4".to_string(), false),
            Token::Comma,
            Token::Number("982268.00".to_string(), false),
            Token::Comma,
            Token::Number("917719.00".to_string(), false),
            Token::Comma,
            Token::SingleQuotedString("Zhongquo".to_string()),
            Token::Comma,
            Token::SingleQuotedString("People\\'sRepublic".to_string()),
            Token::Comma,
            Token::SingleQuotedString("Jiang Zemin".to_string()),
            Token::Comma,
            Token::Number("1891".to_string(), false),
            Token::Comma,
            Token::SingleQuotedString("CN".to_string()),
            Token::RParen,
            Token::SemiColon,
        ];
        assert_eq!(tokens_result.unwrap(), expected);
    }

    #[test]
    fn test_get_column_names_from_insert_into_query() {
        let q = r"
INSERT INTO `customers` (`id`, `first_name`, `last_name`, `email`, `currency`, `accepts_marketing`, `created_at`, `updated_at`)
VALUES (1,'Stanford','Stiedemann','alaina.moore@example.net','EUR',1,'2022-04-13 20:29:23','2022-04-13 20:29:23');
";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert_eq!(tokens_result.is_ok(), true);

        let tokens = trim_pre_whitespaces(tokens_result.unwrap());
        let column_names = get_column_names_from_insert_into_query(&tokens);

        assert_eq!(
            column_names,
            vec![
                "id",
                "first_name",
                "last_name",
                "email",
                "currency",
                "accepts_marketing",
                "created_at",
                "updated_at"
            ]
        );
    }

    #[test]
    fn test_get_column_values_from_insert_into_query() {
        let q = "INSERT INTO `customers` (`id`, `first_name`, `last_name`, `email`, `currency`, `accepts_marketing`, `birthdate`, `created_at`, `updated_at`) VALUES (1,'Stanford','People\\'sRepublic','alaina.moore@example.net','EUR',1,NULL,'2022-04-13 20:29:23','2022-04-13 20:29:23');";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert_eq!(tokens_result.is_ok(), true);

        let tokens = tokens_result.unwrap();
        let tokens = trim_pre_whitespaces(tokens);
        let column_values = get_column_values_from_insert_into_query(&tokens);

        assert_eq!(
            column_values,
            vec![
                &Token::Number("1".to_string(), false),
                &Token::SingleQuotedString("Stanford".to_string()),
                &Token::SingleQuotedString("People\\'sRepublic".to_string()),
                &Token::SingleQuotedString("alaina.moore@example.net".to_string()),
                &Token::SingleQuotedString("EUR".to_string()),
                &Token::Number("1".to_string(), false),
                &Token::make_keyword("NULL"),
                &Token::SingleQuotedString("2022-04-13 20:29:23".to_string()),
                &Token::SingleQuotedString("2022-04-13 20:29:23".to_string()),
            ]
        );
    }

    #[test]
    fn test_match_keyword_at_position() {
        let q = r"
INSERT INTO `customers` (`id`, `first_name`, `last_name`, `email`, `currency`, `accepts_marketing`, `birthdate`, `created_at`, `updated_at`)
VALUES (1,'Stanford','Stiedemann','alaina.moore@example.net','EUR',1,NULL,'2022-04-13 20:29:23','2022-04-13 20:29:23');
";
        let tokens = get_tokens_from_query_str(q);

        assert_eq!(
            match_keyword_at_position(super::Keyword::Insert, &tokens, 0),
            true
        );

        assert_eq!(
            match_keyword_at_position(super::Keyword::Into, &tokens, 2),
            true
        );
    }

    #[test]
    fn test_insert_into_with_boolean_column_type() {
        let q = r"
INSERT INTO `customers` (`first_name`, `is_valid`)
VALUES ('Romaric', true);
";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert_eq!(tokens_result.is_ok(), true);

        let tokens = trim_pre_whitespaces(tokens_result.unwrap());
        let column_values = get_column_values_from_insert_into_query(&tokens);

        assert_eq!(
            column_values,
            vec![
                &Token::SingleQuotedString("Romaric".to_string()),
                &Token::make_word("true", None),
            ]
        );
    }

    #[test]
    fn test_get_single_quoted_string_value_at_position() {
        let q = r"
INSERT INTO `customers` (`first_name`, `is_valid`)
VALUES ('Romaric', true);
";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert_eq!(tokens_result.is_ok(), true);

        let tokens = trim_pre_whitespaces(tokens_result.unwrap());
        assert_eq!(
            "customers",
            get_single_quoted_string_value_at_position(&tokens, 4).unwrap()
        );
        assert!(get_single_quoted_string_value_at_position(&tokens, 0).is_none());
    }

    #[test]
    fn test_insert_into_with_numbers() {
        let q = "INSERT INTO test (postive_number, negative_number, long_number) VALUES (+5.75, -10.20, 20L);";

        let mut tokenizer = Tokenizer::new(q);
        let tokens_result = tokenizer.tokenize();
        assert_eq!(tokens_result.is_ok(), true);

        let tokens = trim_pre_whitespaces(tokens_result.unwrap());
        let column_values = get_column_values_from_insert_into_query(&tokens);

        assert_eq!(
            column_values,
            vec![
                &Token::Number("+5.75".to_string(), false),
                &Token::Number("-10.20".to_string(), false),
                &Token::Number("20".to_string(), true),
            ]
        );
    }
}
