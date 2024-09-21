/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[derive(Clone, Debug)]
pub struct Error {
    messages: Vec<String>,
    // TODO: it would make more sense to use eyre or anyhow for this
    // this vec of strings is just because it's tough to implement
    // `.source()` for `Error`
    cause_messages: Vec<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            messages: vec![message.into()],
            cause_messages: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        Self {
            messages: Default::default(),
            cause_messages: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &Vec<String> {
        &self.messages
    }

    pub fn cause_messages(&self) -> &Vec<String> {
        &self.cause_messages
    }

    pub fn push(&mut self, message: String) {
        self.messages.insert(0, message);
    }
}

impl<E> From<E> for Error
where
    E: std::error::Error,
{
    fn from(error: E) -> Self {
        let mut e: &dyn std::error::Error = &error;
        let messages = vec![e.to_string()];
        let mut cause_messages = Vec::new();
        let mut remaining_trace = 15;
        while let Some(err_source) = e.source() {
            // *really* make sure we don't infinite loop if there are weird .source() issues.
            // octocrab github error sometimes makes itself the source?
            if std::ptr::eq(err_source as *const _, e as *const _)
                || remaining_trace <= 0
            {
                break;
            }
            remaining_trace -= 1;
            cause_messages.push(err_source.to_string());
            e = err_source;
        }
        Self {
            messages,
            cause_messages,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.messages.is_empty() {
            write!(f, "{}", self.messages.join("\n  "))
        } else {
            write!(f, "unknown error")
        }
    }
}

pub trait ResultExt {
    type Output;

    fn convert(self) -> Self::Output;
    fn context(self, message: String) -> Self::Output;
    fn reword(self, message: String) -> Self::Output;
}
impl<T> ResultExt for Result<T> {
    type Output = Self;

    fn convert(self) -> Self {
        self
    }

    fn context(mut self, message: String) -> Self {
        if let Err(error) = &mut self {
            error.push(message);
        }

        self
    }

    fn reword(mut self, message: String) -> Self {
        if let Err(error) = &mut self {
            error.messages.pop();
            error.push(message);
        }

        self
    }
}

impl<T, E> ResultExt for std::result::Result<T, E>
where
    E: std::error::Error,
{
    type Output = Result<T>;

    fn convert(self) -> Result<T> {
        match self {
            Ok(v) => Ok(v),
            Err(error) => Err(error.into()),
        }
    }

    fn context(self, message: String) -> Result<T> {
        match self {
            Ok(v) => Ok(v),
            Err(error) => {
                let mut e = Error::from(error);
                let raw_message = e
                    .messages
                    .pop()
                    .expect("at least one message always exists");
                e.cause_messages.insert(0, raw_message);
                e.messages.push(message);
                Err(e)
            }
        }
    }

    fn reword(self, message: String) -> Result<T> {
        self.convert().reword(message)
    }
}

pub struct Terminator {
    error: Error,
}

impl From<Error> for Terminator {
    fn from(error: Error) -> Self {
        Self { error }
    }
}

impl std::fmt::Debug for Terminator {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "🛑 ")?;
        for message in self.error.messages.iter().rev() {
            writeln!(f, "{}", message)?;
        }
        Ok(())
    }
}

impl<E> From<E> for Terminator
where
    E: std::error::Error,
{
    fn from(error: E) -> Self {
        Self {
            error: error.into(),
        }
    }
}

pub fn add_error<T, U>(result: &mut Result<T>, other: Result<U>) -> Option<U> {
    match other {
        Ok(result) => Some(result),
        Err(error) => {
            if let Err(e) = result {
                e.messages.extend(error.messages);
            } else {
                *result = Err(error);
            }
            None
        }
    }
}
