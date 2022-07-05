use thiserror::Error;

#[derive(Error, Debug)]
pub enum BeakError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

pub type BeakResult<T> = Result<T, BeakError>;
