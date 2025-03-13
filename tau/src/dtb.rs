use core::{fmt, ops::Index, slice, str};

use thiserror_no_std::Error;

pub struct Dtb<'a> {
    dt: &'a [u32],
    str: &'a [u8],
}

const DTB_MAX_DEPTH: usize = 16;

#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    path: DtbPath<'a>,
    pos: usize,
}

impl Cursor<'_> {
    pub const fn empty() -> Self {
        Cursor {
            path: DtbPath::empty(),
            pos: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct DtbPath<'a> {
    size: usize,
    parts: [&'a str; DTB_MAX_DEPTH],
}

impl<'a> DtbPath<'a> {
    const fn empty() -> Self {
        DtbPath {
            size: 0,
            parts: [""; DTB_MAX_DEPTH],
        }
    }

    fn push(&mut self, component: &'a str) {
        unsafe {
            *self.parts.get_mut(self.size).unwrap_unchecked() = component;
        }
        self.size += 1;
    }

    fn pop(&mut self) {
        self.size -= 1;
    }

    fn iter(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().take(self.size).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.size
    }
}

impl<'a> Index<usize> for DtbPath<'a> {
    type Output = &'a str;

    fn index(&self, index: usize) -> &Self::Output {
        self.parts.index(index)
    }
}

pub struct DtbRsv<'a>(&'a [u32]);

pub struct DtbProps<'a> {
    dt: &'a [u32],
    str: &'a [u8],
}

#[derive(Debug, Error)]
pub enum DtbError {
    #[error("dtb header error: {0}")]
    Header(#[from] DtbHeaderError),
}

#[derive(Debug, Error)]
pub enum DtbHeaderError {
    #[error("bad header size")]
    HeaderSize,
    #[error("bad magic")]
    Magic,
    #[error("bad total size")]
    TotalSize,
    #[error("bad reserved memory table size")]
    ReservedMemoryTableSize,
    #[error("bad device tree size")]
    DeviceTreeSize,
    #[error("bad string table size")]
    StringTableSize,
}

const BEGIN_NODE: u32 = 0x00000001;
const END_NODE: u32 = 0x00000002;
const PROP: u32 = 0x00000003;
const NOP: u32 = 0x00000004;
const END: u32 = 0x00000009;

impl<'a> Dtb<'a> {
    const MAGIC: u32 = 0xd00dfeed;

    pub fn new(raw: &'a [u32]) -> Result<(Self, DtbRsv<'a>), DtbHeaderError> {
        let &[
            magic,
            size,
            off_dt,
            off_str,
            off_rsv,
            _,
            _,
            _,
            size_str,
            size_dt,
        ] = raw.first_chunk::<10>().ok_or(DtbHeaderError::HeaderSize)?;

        let parse_size = |size: u32| (size.to_be() as usize) / size_of::<u32>();

        if magic.to_be() != Self::MAGIC {
            return Err(DtbHeaderError::Magic);
        }

        let len = parse_size(size);
        if raw.len() < len {
            return Err(DtbHeaderError::TotalSize);
        }

        let off_rsv = parse_size(off_rsv);
        let rsv = raw
            .get(off_rsv..)
            .ok_or(DtbHeaderError::ReservedMemoryTableSize)?;

        let off_dt = parse_size(off_dt);
        let size_dt = parse_size(size_dt);
        let dt = raw
            .get(off_dt..(off_dt + size_dt))
            .ok_or(DtbHeaderError::DeviceTreeSize)?;

        let off_str = parse_size(off_str);
        let size_str = parse_size(size_str);
        let str = raw
            .get(off_str..(off_str + size_str))
            .ok_or(DtbHeaderError::StringTableSize)?;
        let str =
            unsafe { slice::from_raw_parts(str.as_ptr().cast(), size_str * size_of::<u32>()) };

        Ok((Dtb { dt, str }, DtbRsv(rsv)))
    }

    pub fn iter<'b>(&'b self) -> DtbIter<'b> {
        DtbIter {
            inner: Dtb {
                dt: self.dt,
                str: self.str,
            },
            cursor: Cursor::empty(),
        }
    }
}

impl Iterator for DtbRsv<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        let &[hi_addr, lo_addr, hi_size, lo_size] = self.0.first_chunk()?;
        if hi_addr == 0 && lo_addr == 0 && hi_size == 0 && lo_size == 0 {
            return None;
        }
        let addr = ((hi_addr.to_be() as usize) << 32) + (lo_addr.to_be() as usize);
        let size = ((hi_size.to_be() as usize) << 32) + (lo_size.to_be() as usize);
        Some((addr, size))
    }
}

pub struct DtbIter<'a> {
    inner: Dtb<'a>,
    cursor: Cursor<'a>,
}

impl<'a> DtbIter<'a> {
    fn lookahead(&self) -> Option<u32> {
        self.inner.dt.get(self.cursor.pos).copied().map(u32::to_be)
    }

    fn advance(&mut self, len: usize) {
        self.cursor.pos += len;
    }
}

impl<'a> Iterator for DtbIter<'a> {
    type Item = (DtbProps<'a>, DtbPath<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let mut start = None;
        let cursor = loop {
            match self.lookahead()? {
                BEGIN_NODE => {
                    if let Some(start) = start {
                        break start;
                    }
                    self.advance(1);

                    let start_ptr =
                        (self.inner.dt.get(self.cursor.pos)? as *const u32).cast::<u8>();
                    loop {
                        let stop = self.lookahead()? & 0xff == 0;
                        self.advance(1);
                        if stop {
                            break;
                        }
                    }
                    let end_ptr = (self.inner.dt.get(self.cursor.pos)? as *const u32).cast::<u8>();
                    let str = unsafe {
                        let bytes = slice::from_raw_parts(
                            start_ptr,
                            end_ptr.offset_from(start_ptr) as usize,
                        );
                        str::from_utf8(bytes)
                            .unwrap_unchecked()
                            .trim_end_matches('\0')
                    };
                    self.cursor.path.push(str);
                }
                END_NODE => {
                    if let Some(start) = start {
                        break start;
                    }

                    self.advance(1);
                    self.cursor.path.pop();
                }
                PROP => {
                    if start.is_none() {
                        start = Some(self.cursor);
                    }
                    self.advance(1);
                    let len = (self.lookahead()? as usize).div_ceil(size_of::<u32>());
                    self.advance(2 + len); // len, name, data
                }
                NOP => self.advance(1),
                END => {
                    if let Some(start) = start {
                        break start;
                    }
                    self.advance(1)
                }
                _ => return None,
            }
        };
        Some((
            DtbProps {
                dt: self.inner.dt.get(cursor.pos..self.cursor.pos)?,
                str: self.inner.str,
            },
            cursor.path,
        ))
    }
}

impl<'a> DtbProps<'a> {
    pub fn find_str<P>(&self, predicate: P) -> Option<&str>
    where
        P: Fn(&str) -> bool,
    {
        self.find(predicate)
            .and_then(|b| str::from_utf8(b).ok())
            .map(|s| s.trim_end_matches('\0'))
    }

    pub fn find_int<P>(&self, predicate: P) -> Option<&[u32]>
    where
        P: Fn(&str) -> bool,
    {
        self.find(predicate).and_then(|b| {
            (b.len() % size_of::<u32>() == 0).then(|| unsafe {
                slice::from_raw_parts(b.as_ptr().cast::<u32>(), b.len() / size_of::<u32>())
            })
        })
    }

    fn find<P>(&self, predicate: P) -> Option<&[u8]>
    where
        P: Fn(&str) -> bool,
    {
        let mut cursor = 0;
        let get = |c| self.dt.get(c).copied().map(u32::to_be);
        loop {
            match get(cursor)? {
                PROP => {
                    cursor += 1;
                    let len = (get(cursor)? as usize).div_ceil(size_of::<u32>());
                    cursor += 1;
                    let name = get(cursor)? as usize;
                    cursor += 1;
                    let data = if len == 0 {
                        &[]
                    } else {
                        let ptr = (self.dt.get(cursor)? as *const u32).cast::<u8>();
                        cursor += len;

                        unsafe { slice::from_raw_parts(ptr, len * size_of::<u32>()) }
                    };

                    let str_from_u8_nul_utf8_unchecked = |utf8_src: &'a [u8]| {
                        let nul_range_end = utf8_src
                            .iter()
                            .position(|&c| c == b'\0')
                            .unwrap_or(utf8_src.len()); // default to length if no `\0` present
                        unsafe { str::from_utf8_unchecked(&utf8_src[0..nul_range_end]) }
                    };
                    let key = str_from_u8_nul_utf8_unchecked(self.str.get(name..)?);
                    if predicate(key) {
                        break Some(data);
                    }
                }
                NOP => cursor += 1,
                a => crate::dbg([a as usize, 0xdeadbeef]),
            }
        }
    }
}

impl fmt::Display for DtbPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for component in self.iter() {
            write!(f, "{component}/")?;
        }

        Ok(())
    }
}
