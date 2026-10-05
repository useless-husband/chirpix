//! chirpix: a picture sent through the air as sound.
//!
//! Everything in the signal path is in this crate: the OFDM modem
//! ([`modem`]), convolutional code and Viterbi decoder ([`conv`]), the
//! expanding-window fountain code ([`fountain`]), the progressive wavelet
//! image codec ([`codec`]), and the WAV / PNG / DEFLATE file formats.

// Signal-processing loops here index several arrays by the same carrier or
// sample number; iterator chains would hide that.
#![allow(clippy::needless_range_loop)]

pub mod channel;
pub mod chart;
pub mod codec;
pub mod conv;
pub mod deflate;
pub mod dsp;
pub mod experiments;
pub mod fft;
pub mod fountain;
pub mod image;
pub mod link;
pub mod modem;
pub mod png;
pub mod rangecoder;
pub mod report;
pub mod util;
pub mod wav;
pub mod wavelet;
