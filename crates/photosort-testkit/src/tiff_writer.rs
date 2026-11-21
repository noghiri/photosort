//! Serializes TIFF structures for synthetic test files.
//!
//! Layout runs in two passes. The plan pass assigns every IFD table, every
//! out-of-line value and every blob an offset. The write pass then emits
//! bytes, and pointer values are read from the plan.
// pattern: Functional Core

use crate::spec::Order;

const HEADER_LEN: usize = 8;

/// Builds one IFD. Entries are kept sorted by tag.
#[derive(Debug, Clone, Default)]
pub struct IfdBuilder {
    entries: Vec<(u16, Value)>,
}

#[derive(Debug, Clone)]
enum Value {
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<(u32, u32)>),
    Undefined(Vec<u8>),
    /// LONG holding the offset of one child IFD.
    Child(IfdBuilder),
    /// LONG array holding the offsets of child IFDs.
    Children(Vec<IfdBuilder>),
    /// LONG holding the offset of a blob placed after all IFDs.
    BlobOffset(Vec<u8>),
    /// LONG holding the length of a blob. Writes no data.
    BlobLen(u32),
}

impl IfdBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn ascii(self, tag: u16, text: &str) -> Self {
        self.with(tag, Value::Ascii(text.to_owned()))
    }

    #[must_use]
    pub fn short(self, tag: u16, values: &[u16]) -> Self {
        self.with(tag, Value::Short(values.to_vec()))
    }

    #[must_use]
    pub fn long(self, tag: u16, values: &[u32]) -> Self {
        self.with(tag, Value::Long(values.to_vec()))
    }

    #[must_use]
    pub fn rational(self, tag: u16, values: &[(u32, u32)]) -> Self {
        self.with(tag, Value::Rational(values.to_vec()))
    }

    #[must_use]
    pub fn undefined(self, tag: u16, bytes: &[u8]) -> Self {
        self.with(tag, Value::Undefined(bytes.to_vec()))
    }

    #[must_use]
    pub fn child(self, tag: u16, ifd: IfdBuilder) -> Self {
        self.with(tag, Value::Child(ifd))
    }

    #[must_use]
    pub fn children(self, tag: u16, ifds: Vec<IfdBuilder>) -> Self {
        self.with(tag, Value::Children(ifds))
    }

    /// Stores `blob` once in the file (identical blobs share one copy) and
    /// writes its offset under `tag`.
    #[must_use]
    pub fn blob_offset(self, tag: u16, blob: &[u8]) -> Self {
        self.with(tag, Value::BlobOffset(blob.to_vec()))
    }

    /// Writes the length of `blob` under `tag`. The bytes are not stored;
    /// pair with `blob_offset` on the same blob to store them.
    #[must_use]
    pub fn blob_len(self, tag: u16, blob: &[u8]) -> Self {
        self.with(tag, Value::BlobLen(count(blob.len())))
    }

    fn with(mut self, tag: u16, value: Value) -> Self {
        let at = self.entries.partition_point(|(t, _)| *t < tag);
        self.entries.insert(at, (tag, value));
        self
    }
}

/// Where a value's bytes sit in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueSpan {
    pub tag: u16,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffBytes {
    pub bytes: Vec<u8>,
    /// Offset of each blob, in placement order: referenced blobs (by first
    /// use in DFS order) first, then the free blobs passed in.
    pub blob_offsets: Vec<usize>,
    /// Every entry's value span, in DFS order of IFDs.
    pub values: Vec<ValueSpan>,
}

/// Serializes a TIFF with the header at offset 0 and IFD0 at offset 8.
/// `tail_padding` zero bytes precede the blobs.
#[must_use]
pub fn write_tiff(order: Order, magic: u16, ifd0: &IfdBuilder, tail_padding: usize) -> TiffBytes {
    write_tiff_with_blobs(order, magic, ifd0, tail_padding, &[])
}

/// As `write_tiff`, also placing `free_blobs` after the referenced blobs.
/// Use this for bytes that another structure points at through a
/// maker note rather than through a tag in this TIFF.
#[must_use]
pub fn write_tiff_with_blobs(
    order: Order,
    magic: u16,
    ifd0: &IfdBuilder,
    tail_padding: usize,
    free_blobs: &[Vec<u8>],
) -> TiffBytes {
    let mut layout = Layout::default();
    let tree_end = plan_ifd(ifd0, HEADER_LEN, &mut layout);

    let mut cursor = tree_end + tail_padding;
    let mut blob_offsets = Vec::new();
    for blob in layout.blobs.iter().chain(free_blobs) {
        cursor = align2(cursor);
        blob_offsets.push(cursor);
        cursor += blob.len();
    }

    let mut bytes = vec![0u8; cursor];
    let mark: &[u8] = match order {
        Order::Little => b"II",
        Order::Big => b"MM",
    };
    put(&mut bytes, 0, mark);
    put(&mut bytes, 2, &enc16(order, magic));
    put(&mut bytes, 4, &enc32(order, count(HEADER_LEN)));

    let ctx = Ctx {
        order,
        layout: &layout,
        blob_offsets: &blob_offsets,
    };
    let mut values = Vec::new();
    write_ifd(ifd0, 0, &ctx, &mut bytes, &mut values);

    for (blob, at) in layout.blobs.iter().chain(free_blobs).zip(&blob_offsets) {
        put(&mut bytes, *at, blob);
    }

    TiffBytes {
        bytes,
        blob_offsets,
        values,
    }
}

#[derive(Debug, Default)]
struct Plan {
    table_at: usize,
    /// Position of each entry's value: its data when out of line, else
    /// the entry's own 4-byte value field.
    value_at: Vec<usize>,
    /// Plan indices of the child IFDs of each entry (empty for other entries).
    children: Vec<Vec<usize>>,
}

#[derive(Debug, Default)]
struct Layout {
    /// Pre-order, so index 0 is IFD0.
    plans: Vec<Plan>,
    /// Distinct blobs in first-use order.
    blobs: Vec<Vec<u8>>,
}

struct Ctx<'a> {
    order: Order,
    layout: &'a Layout,
    blob_offsets: &'a [usize],
}

impl Ctx<'_> {
    fn blob_at(&self, bytes: &[u8]) -> usize {
        let index = self
            .layout
            .blobs
            .iter()
            .position(|b| b.as_slice() == bytes)
            .expect("every referenced blob was planned");
        self.blob_offsets[index]
    }
}

/// Returns the offset just past this IFD and everything below it.
fn plan_ifd(ifd: &IfdBuilder, at: usize, layout: &mut Layout) -> usize {
    let me = layout.plans.len();
    layout.plans.push(Plan::default());

    let mut cur = at + 2 + 12 * ifd.entries.len() + 4;
    let mut value_at = Vec::with_capacity(ifd.entries.len());
    for (i, (_, value)) in ifd.entries.iter().enumerate() {
        let (_, _, len) = value.shape();
        if len <= 4 {
            value_at.push(at + 2 + 12 * i + 8);
        } else {
            cur = align2(cur);
            value_at.push(cur);
            cur += len;
        }
        if let Value::BlobOffset(bytes) = value
            && !layout.blobs.contains(bytes)
        {
            layout.blobs.push(bytes.clone());
        }
    }

    let mut children = Vec::with_capacity(ifd.entries.len());
    for (_, value) in &ifd.entries {
        let mut indices = Vec::new();
        let kids: Vec<&IfdBuilder> = match value {
            Value::Child(c) => vec![c],
            Value::Children(v) => v.iter().collect(),
            _ => Vec::new(),
        };
        for kid in kids {
            indices.push(layout.plans.len());
            cur = plan_ifd(kid, align2(cur), layout);
        }
        children.push(indices);
    }

    layout.plans[me] = Plan {
        table_at: at,
        value_at,
        children,
    };
    cur
}

fn write_ifd(
    ifd: &IfdBuilder,
    idx: usize,
    ctx: &Ctx<'_>,
    out: &mut [u8],
    spans: &mut Vec<ValueSpan>,
) {
    let plan = &ctx.layout.plans[idx];
    put(
        out,
        plan.table_at,
        &enc16(ctx.order, count16(ifd.entries.len())),
    );

    for (i, (tag, value)) in ifd.entries.iter().enumerate() {
        let (field_type, n, len) = value.shape();
        let pointers: Vec<u32> = match value {
            Value::Child(_) | Value::Children(_) => plan.children[i]
                .iter()
                .map(|&c| count(ctx.layout.plans[c].table_at))
                .collect(),
            Value::BlobOffset(bytes) => vec![count(ctx.blob_at(bytes))],
            _ => Vec::new(),
        };
        let data = value.data(ctx.order, &pointers);

        let entry_at = plan.table_at + 2 + 12 * i;
        let value_at = plan.value_at[i];
        put(out, entry_at, &enc16(ctx.order, *tag));
        put(out, entry_at + 2, &enc16(ctx.order, field_type));
        put(out, entry_at + 4, &enc32(ctx.order, n));
        if len <= 4 {
            put(out, entry_at + 8, &data);
        } else {
            put(out, value_at, &data);
            put(out, entry_at + 8, &enc32(ctx.order, count(value_at)));
        }
        spans.push(ValueSpan {
            tag: *tag,
            offset: value_at,
            len,
        });
    }

    for (i, (_, value)) in ifd.entries.iter().enumerate() {
        let kids: Vec<&IfdBuilder> = match value {
            Value::Child(c) => vec![c],
            Value::Children(v) => v.iter().collect(),
            _ => Vec::new(),
        };
        for (kid, &kid_idx) in kids.into_iter().zip(&plan.children[i]) {
            write_ifd(kid, kid_idx, ctx, out, spans);
        }
    }
}

impl Value {
    /// (field type, count, data length in bytes).
    fn shape(&self) -> (u16, u32, usize) {
        match self {
            Value::Ascii(s) => (2, count(s.len() + 1), s.len() + 1),
            Value::Short(v) => (3, count(v.len()), 2 * v.len()),
            Value::Long(v) => (4, count(v.len()), 4 * v.len()),
            Value::Rational(v) => (5, count(v.len()), 8 * v.len()),
            Value::Undefined(v) => (7, count(v.len()), v.len()),
            Value::Child(_) | Value::BlobOffset(_) | Value::BlobLen(_) => (4, 1, 4),
            Value::Children(v) => (4, count(v.len()), 4 * v.len()),
        }
    }

    /// Serialized data. `pointers` holds child-IFD or blob offsets, one per
    /// child (or one for a blob).
    fn data(&self, order: Order, pointers: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Value::Ascii(s) => {
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Value::Short(v) => {
                for x in v {
                    out.extend(enc16(order, *x));
                }
            }
            Value::Long(v) => {
                for x in v {
                    out.extend(enc32(order, *x));
                }
            }
            Value::Rational(v) => {
                for (num, den) in v {
                    out.extend(enc32(order, *num));
                    out.extend(enc32(order, *den));
                }
            }
            Value::Undefined(v) => out.extend_from_slice(v),
            Value::Child(_) | Value::BlobOffset(_) => {
                out.extend(enc32(order, pointers.first().copied().unwrap_or(0)));
            }
            Value::Children(_) => {
                for p in pointers {
                    out.extend(enc32(order, *p));
                }
            }
            Value::BlobLen(n) => out.extend(enc32(order, *n)),
        }
        out
    }
}

fn align2(n: usize) -> usize {
    n + (n % 2)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).expect("count fits in u32")
}

fn count16(n: usize) -> u16 {
    u16::try_from(n).expect("entry count fits in u16")
}

fn enc16(order: Order, v: u16) -> [u8; 2] {
    match order {
        Order::Little => v.to_le_bytes(),
        Order::Big => v.to_be_bytes(),
    }
}

fn enc32(order: Order, v: u32) -> [u8; 4] {
    match order {
        Order::Little => v.to_le_bytes(),
        Order::Big => v.to_be_bytes(),
    }
}

fn put(out: &mut [u8], at: usize, src: &[u8]) {
    out[at..at + src.len()].copy_from_slice(src);
}
