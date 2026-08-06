//! The crate's error type.
//!
//! Named variants rather than formatted strings, so callers can match. The JS
//! boundary flattens them to messages; the Rust side keeps the structure, which
//! is how the supervisor tells "device disappeared, reopen" from "configuration
//! is impossible, give up".

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Nothing plugged in, or no audio on this machine at all.
    #[error("no default output device")]
    NoOutputDevice,

    /// The device exists but won't report a usable configuration.
    #[error("could not read the default output device configuration")]
    DeviceConfig(#[source] cpal::Error),

    /// The device could not be identified, so device-change detection is
    /// unavailable. Capture can still proceed.
    #[error("could not identify the output device")]
    DeviceId(#[source] cpal::Error),

    /// On macOS this is distinct from a denied permission, which succeeds and
    /// then delivers silence.
    #[error("could not open a loopback stream on {device}")]
    OpenStream {
        device: String,
        #[source]
        source: cpal::Error,
    },

    #[error("could not start the loopback stream")]
    StartStream(#[source] cpal::Error),

    /// cpal offered a sample format this crate has no conversion for.
    #[error("device sample format {0} is not supported")]
    UnsupportedSampleFormat(cpal::SampleFormat),

    #[error("could not build a resampler for {from} Hz to {to} Hz")]
    Resampler {
        from: u32,
        to: u32,
        #[source]
        source: rubato::ResamplerConstructionError,
    },

    #[error("sample rate must be greater than zero")]
    ZeroSampleRate,

    /// A thread panicked holding shared state; the buffer may be half-written.
    #[error("shared state was poisoned by a panic on another thread")]
    Poisoned,

    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),

    #[error("buffer is frozen; call resume() to capture again")]
    Frozen,

    #[error("buffer is not running")]
    NotRunning,

    #[error("buffer is not frozen; call start()")]
    NotFrozen,
}

impl Error {
    /// The message with every underlying cause appended.
    ///
    /// The JS boundary carries only a string and the useful half is usually the
    /// cpal error underneath. Separate from the `napi` conversion so it is
    /// testable without linking against the Node host.
    pub fn message_chain(&self) -> String {
        let mut message = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        message
    }
}

impl From<Error> for napi::Error {
    fn from(err: Error) -> Self {
        napi::Error::new(napi::Status::GenericFailure, err.message_chain())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_lowercase() {
        for err in [
            Error::NoOutputDevice,
            Error::ZeroSampleRate,
            Error::Poisoned,
            Error::NotRunning,
            Error::NotFrozen,
        ] {
            let msg = err.to_string();
            let first = msg.chars().next().expect("error messages are never empty");
            assert!(!first.is_uppercase(), "{msg:?} starts with a capital");
            assert!(!msg.ends_with('.'), "{msg:?} ends with a full stop");
        }
    }

    #[test]
    fn message_chain_keeps_cause() {
        let err = Error::Resampler {
            from: 44_100,
            to: 16_000,
            source: rubato::ResamplerConstructionError::InvalidSampleRate {
                input: 0,
                output: 0,
            },
        };

        let message = err.message_chain();
        let top_level = err.to_string();

        assert!(message.contains("44100"), "lost the context: {message}");
        assert!(
            message.len() > top_level.len(),
            "source chain was dropped: {message}"
        );
    }

    #[test]
    fn causeless_error_chains_to_itself() {
        assert_eq!(
            Error::NoOutputDevice.message_chain(),
            Error::NoOutputDevice.to_string()
        );
    }

    #[test]
    fn state_errors_name_the_next_call() {
        assert!(Error::Frozen.to_string().contains("resume()"));
        assert!(Error::NotFrozen.to_string().contains("start()"));
    }
}
