use crate::ng::Encodable;
use std::{

    io::{Result, Write},
};

use super::ByteWriter;

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq, Hash)]


#[cfg_attr(feature = "serde", derive(::serde::Deserialize))]
pub struct Unvip<'a> {
    pub 
    channel: &'a str,
    pub 
    username: &'a str,
}

pub fn unvip<'a>(channel: &'a str, username: &'a str) -> Unvip<'a> {
    Unvip { channel, username }
}

impl<'a> Encodable for Unvip<'a> {
    fn encode<W: Write + ?Sized>(&self, buf: &mut W) -> Result<()> {
        ByteWriter::new(buf).command(self.channel, &[&"/unvip", &self.username])
    }
}