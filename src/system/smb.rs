//! Producer side of Perfetto's shared memory ABI
//! (`include/perfetto/ext/tracing/core/shared_memory_abi.h`, v53.1).
//!
//! Layout: the buffer is split in pages of `page_size` bytes. Each page starts
//! with an 8-byte `PageHeader` whose 32-bit atomic bitmap holds the page
//! layout (bits 28..30) and a 2-bit state per chunk (bits 0..27). We always
//! partition a page as `kPageDiv1` (one chunk of `align4(page_size - 8)`
//! bytes). Each chunk starts with an 8-byte `ChunkHeader`
//! `{u32 chunk_id; u16 writer_id; u16 packets(count:10, flags:6)}` followed by
//! packet fragments, each prefixed by a 4-byte redundant varint length.

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};

pub const PAGE_HEADER_SIZE: usize = 8;
pub const CHUNK_HEADER_SIZE: usize = 8;
pub const PACKET_HEADER_SIZE: usize = 4;
pub const MAX_PACKETS_PER_CHUNK: u16 = (1 << 10) - 1;

const LAYOUT_SHIFT: u32 = 28;
const LAYOUT_DIV1: u32 = 1;
const CHUNK_FREE: u32 = 0;
const CHUNK_BEING_WRITTEN: u32 = 1;
const CHUNK_COMPLETE: u32 = 3;

pub const FLAG_FIRST_PACKET_CONTINUES_FROM_PREV: u8 = 1 << 0;
pub const FLAG_LAST_PACKET_CONTINUES_ON_NEXT: u8 = 1 << 1;

pub struct Smb {
    base: *mut u8,
    size: usize,
    page_size: usize,
    num_pages: usize,
    next_page: AtomicUsize,
    owned: Option<Vec<u64>>,
}

// The buffer is only accessed through atomics (headers) or by the single
// writer that owns a chunk in kChunkBeingWritten state.
unsafe impl Send for Smb {}
unsafe impl Sync for Smb {}

/// A chunk owned by a writer.
#[derive(Debug)]
pub struct ChunkCursor {
    pub page: u32,
    pub used: usize,
    pub count: u16,
    pub flags: u8,
}

impl Smb {
    /// Maps the shared memory fd received from traced.
    #[cfg(unix)]
    pub fn map_fd(fd: i32, page_size: usize) -> std::io::Result<Smb> {
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let size = st.st_size as usize;
            if size == 0 || page_size < 4096 || size % page_size != 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad SMB size"));
            }
            let p = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Smb {
                base: p as *mut u8,
                size,
                page_size,
                num_pages: size / page_size,
                next_page: AtomicUsize::new(0),
                owned: None,
            })
        }
    }

    /// Heap-backed buffer (tests).
    pub fn new_heap(size: usize, page_size: usize) -> Smb {
        let mut v = vec![0u64; size / 8];
        let base = v.as_mut_ptr() as *mut u8;
        Smb { base, size, page_size, num_pages: size / page_size, next_page: AtomicUsize::new(0), owned: Some(v) }
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn num_pages(&self) -> usize {
        self.num_pages
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Bytes available for packet data in one chunk.
    pub fn payload_size(&self) -> usize {
        ((self.page_size - PAGE_HEADER_SIZE) & !3) - CHUNK_HEADER_SIZE
    }

    fn page_header(&self, page: usize) -> &AtomicU32 {
        unsafe { &*(self.base.add(page * self.page_size) as *const AtomicU32) }
    }

    fn chunk_base(&self, page: usize) -> *mut u8 {
        unsafe { self.base.add(page * self.page_size + PAGE_HEADER_SIZE) }
    }

    pub fn page_bitmap(&self, page: usize) -> u32 {
        self.page_header(page).load(Ordering::Acquire)
    }

    /// Takes a free page, partitions it as one chunk in BeingWritten state and
    /// initializes the chunk header.
    pub fn acquire(&self, writer_id: u16, chunk_id: u32) -> Option<ChunkCursor> {
        let start = self.next_page.load(Ordering::Relaxed);
        for i in 0..self.num_pages {
            let page = (start + i) % self.num_pages;
            let hdr = self.page_header(page);
            if hdr.load(Ordering::Relaxed) != 0 {
                continue;
            }
            let next = (LAYOUT_DIV1 << LAYOUT_SHIFT) | CHUNK_BEING_WRITTEN;
            if hdr.compare_exchange(0, next, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                self.next_page.store(page + 1, Ordering::Relaxed);
                let c = self.chunk_base(page);
                unsafe {
                    (*(c as *const AtomicU32)).store(chunk_id, Ordering::Relaxed);
                    (*(c.add(4) as *const AtomicU16)).store(writer_id, Ordering::Relaxed);
                    (*(c.add(6) as *const AtomicU16)).store(0, Ordering::Release);
                }
                return Some(ChunkCursor { page: page as u32, used: 0, count: 0, flags: 0 });
            }
        }
        None
    }

    /// Appends one packet fragment. Caller guarantees it fits.
    pub fn write_fragment(&self, cur: &mut ChunkCursor, data: &[u8]) {
        debug_assert!(cur.used + PACKET_HEADER_SIZE + data.len() <= self.payload_size());
        let c = self.chunk_base(cur.page as usize);
        unsafe {
            let dst = c.add(CHUNK_HEADER_SIZE + cur.used);
            let mut len = [0u8; 4];
            let n = data.len();
            len[0] = (n as u8 & 0x7f) | 0x80;
            len[1] = ((n >> 7) as u8 & 0x7f) | 0x80;
            len[2] = ((n >> 14) as u8 & 0x7f) | 0x80;
            len[3] = (n >> 21) as u8 & 0x7f;
            std::ptr::copy_nonoverlapping(len.as_ptr(), dst, 4);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst.add(4), n);
        }
        cur.used += PACKET_HEADER_SIZE + data.len();
        cur.count += 1;
        self.store_packets(cur);
    }

    fn store_packets(&self, cur: &ChunkCursor) {
        let c = self.chunk_base(cur.page as usize);
        let v = (cur.count & MAX_PACKETS_PER_CHUNK) | ((cur.flags as u16) << 10);
        unsafe { (*(c.add(6) as *const AtomicU16)).store(v, Ordering::Release) };
    }

    /// Marks the chunk complete (BeingWritten → Complete). After this the
    /// service owns it until it frees the page.
    pub fn complete(&self, cur: &ChunkCursor) {
        self.store_packets(cur);
        let hdr = self.page_header(cur.page as usize);
        let mut bm = hdr.load(Ordering::Relaxed);
        loop {
            debug_assert_eq!(bm & 3, CHUNK_BEING_WRITTEN);
            let next = (bm & !3) | CHUNK_COMPLETE;
            match hdr.compare_exchange_weak(bm, next, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return,
                Err(x) => bm = x,
            }
        }
    }

    /// Service-side release (tests only): Complete → Free, de-partitioning
    /// the page.
    pub fn service_release(&self, page: usize) {
        self.page_header(page).store(CHUNK_FREE, Ordering::Release);
    }

    /// Reads back a chunk: (chunk_id, writer_id, count, flags, payload).
    pub fn read_chunk(&self, page: usize) -> (u32, u16, u16, u8, &[u8]) {
        let c = self.chunk_base(page);
        unsafe {
            let id = (*(c as *const AtomicU32)).load(Ordering::Acquire);
            let wid = (*(c.add(4) as *const AtomicU16)).load(Ordering::Acquire);
            let p = (*(c.add(6) as *const AtomicU16)).load(Ordering::Acquire);
            let payload = std::slice::from_raw_parts(c.add(CHUNK_HEADER_SIZE), self.payload_size());
            (id, wid, p & MAX_PACKETS_PER_CHUNK, (p >> 10) as u8, payload)
        }
    }
}

impl Drop for Smb {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.owned.is_none() {
            unsafe { libc::munmap(self.base as *mut libc::c_void, self.size) };
        }
    }
}

/// Splits `data` into chunk fragments, acquiring chunks from `smb` as needed.
/// `on_complete` is called for every chunk that becomes complete. Returns
/// false if the SMB ran out of free pages (the packet is dropped; a partially
/// written packet is left flagged as continuing, which the service discards).
pub fn write_packet(
    smb: &Smb,
    cur: &mut Option<ChunkCursor>,
    writer_id: u16,
    next_chunk_id: &mut u32,
    data: &[u8],
    mut on_complete: impl FnMut(&ChunkCursor),
) -> bool {
    let cap = smb.payload_size();
    let mut rem = data;
    let mut first = true;
    loop {
        if let Some(c) = cur.as_ref() {
            let space = cap - c.used;
            let need_whole = PACKET_HEADER_SIZE + rem.len();
            let no_room = space <= PACKET_HEADER_SIZE || c.count >= MAX_PACKETS_PER_CHUNK;
            // Avoid fragmenting a packet that would fit in a fresh chunk.
            let better_fresh = first && need_whole > space && need_whole <= cap && c.count > 0;
            if no_room || better_fresh {
                let c = cur.take().unwrap();
                smb.complete(&c);
                on_complete(&c);
            }
        }
        if cur.is_none() {
            match smb.acquire(writer_id, *next_chunk_id) {
                Some(mut c) => {
                    *next_chunk_id = next_chunk_id.wrapping_add(1);
                    if !first {
                        c.flags |= FLAG_FIRST_PACKET_CONTINUES_FROM_PREV;
                    }
                    *cur = Some(c);
                }
                None => return false,
            }
        }
        let c = cur.as_mut().unwrap();
        let space = cap - c.used - PACKET_HEADER_SIZE;
        let take = rem.len().min(space);
        smb.write_fragment(c, &rem[..take]);
        rem = &rem[take..];
        if rem.is_empty() {
            return true;
        }
        c.flags |= FLAG_LAST_PACKET_CONTINUES_ON_NEXT;
        let c = cur.take().unwrap();
        smb.complete(&c);
        on_complete(&c);
        first = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_fragments(payload: &[u8], count: u16) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut pos = 0;
        for _ in 0..count {
            let mut p = pos;
            let len = prost::encoding::decode_varint(&mut &payload[p..p + 4]).unwrap() as usize;
            p += 4;
            out.push(payload[p..p + len].to_vec());
            pos = p + len;
        }
        out
    }

    #[test]
    fn layout_and_states() {
        let smb = Smb::new_heap(4096 * 4, 4096);
        assert_eq!(smb.payload_size(), 4096 - 8 - 8);
        let c = smb.acquire(7, 42).unwrap();
        assert_eq!(smb.page_bitmap(0), (1 << 28) | 1);
        let mut c = c;
        smb.write_fragment(&mut c, b"hello");
        smb.complete(&c);
        assert_eq!(smb.page_bitmap(0), (1 << 28) | 3);
        let (id, wid, count, flags, payload) = smb.read_chunk(0);
        assert_eq!((id, wid, count, flags), (42, 7, 1, 0));
        assert_eq!(read_fragments(payload, 1), vec![b"hello".to_vec()]);
    }

    #[test]
    fn fragmentation_across_chunks() {
        let smb = Smb::new_heap(4096 * 8, 4096);
        let mut cur = None;
        let mut next_id = 0;
        let mut completed = Vec::new();
        let small = vec![1u8; 100];
        assert!(write_packet(&smb, &mut cur, 3, &mut next_id, &small, |c| completed.push(c.page)));
        let big: Vec<u8> = (0..10000u32).map(|i| i as u8).collect();
        assert!(write_packet(&smb, &mut cur, 3, &mut next_id, &big, |c| completed.push(c.page)));
        let last = cur.take().unwrap();
        smb.complete(&last);
        completed.push(last.page);
        assert_eq!(completed.len(), 3);
        // Reassemble.
        let mut joined = Vec::new();
        let mut packets = Vec::new();
        for (i, &page) in completed.iter().enumerate() {
            let (id, _, count, flags, payload) = smb.read_chunk(page as usize);
            assert_eq!(id, i as u32);
            let frags = read_fragments(payload, count);
            for (j, f) in frags.into_iter().enumerate() {
                let cont_prev = j == 0 && flags & FLAG_FIRST_PACKET_CONTINUES_FROM_PREV != 0;
                if cont_prev {
                    joined.extend_from_slice(&f);
                } else {
                    if !joined.is_empty() {
                        packets.push(std::mem::take(&mut joined));
                    }
                    joined = f;
                }
            }
        }
        packets.push(joined);
        assert_eq!(packets, vec![small, big]);
    }

    #[test]
    fn exhaustion_and_reuse() {
        let smb = Smb::new_heap(4096 * 2, 4096);
        let a = smb.acquire(1, 0).unwrap();
        let b = smb.acquire(1, 1).unwrap();
        assert!(smb.acquire(1, 2).is_none());
        smb.complete(&a);
        smb.service_release(a.page as usize);
        assert!(smb.acquire(1, 2).is_some());
        let _ = b;
    }
}
