/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[derive(Clone, Debug)]
pub struct Error {
    messages: Vec<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            messages: vec![message.into()],
        }
    }

    pub fn empty() -> Self {
        Self {
            messages: Default::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &Vec<String> {
        &self.messages
    }

    pub fn push(&mut self, message: String) {
        self.messages.push(message);
    }
}

impl<E> From<E> for Error
where
    E: std::error::Error,
{
    fn from(error: E) -> Self {
        Self {
            messages: vec![format!("{}", error)],
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = self.messages.last();
        if let Some(message) = message {
            write!(f, "{}", message)
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
        self.convert().context(message)
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
