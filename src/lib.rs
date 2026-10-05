//! chirpix: a picture sent through the air as sound.
//!
//! Everything in the signal path is in this crate: the OFDM modem
//! ([`modem`]), convolutional code and Viterbi decoder ([`conv`]), the
//! expanding-window fountain code ([`fountain`]), the progressive wavelet
//! image codec ([`codec`]), and the WAV / PNG / DEFLATE file formats.

pub mod codec;
pub mod conv;
pub mod deflate;
pub mod fft;
pub mod fountain;
pub mod image;
pub mod png;
pub mod rangecoder;
pub mod util;
pub mod wav;
pub mod wavelet;
