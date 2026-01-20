use core::mem::{offset_of, size_of};

pub const PAYLOAD_MAGIC: &[u8; 6] = b"KNAT20";

pub const SPK_BYTES_MIN: usize = 36;
pub const SPK_BYTES_MAX: usize = 37;
pub const ASSET_ID_SIZE: usize = 36;

pub const MAX_INPUTS_COUNT: usize = 2;
pub const MAX_OUTPUTS_COUNT: usize = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SpkEntry {
    len: u8,
    // spk can be 37 | 36
    bytes: [u8; SPK_BYTES_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MintPayloadHeader {
    magic: [u8; 6],
    asset_id: [u8; ASSET_ID_SIZE],
    authority_spk: SpkEntry,
    token_spk: SpkEntry,
    remaining_supply: [u8; 8], // u64 LE
    op: u8,
    total_amount: [u8; 8], // u64 LE
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TransferPayloadHeader {
    magic: [u8; 6],
    asset_id: [u8; ASSET_ID_SIZE],
    authority_spk: SpkEntry,
    token_spk: SpkEntry,
    op: u8,
    total_amount: [u8; 8], // u64 LE
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MintPayloadLayout {
    header: MintPayloadHeader,
    output0_amount: [u8; 8], // u64 LE
    output0_recipient: SpkEntry,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TransferPayloadLayout {
    header: TransferPayloadHeader,
    input_amounts: [[u8; 8]; MAX_INPUTS_COUNT],   // u64 LE
    output_amounts: [[u8; 8]; MAX_OUTPUTS_COUNT], // u64 LE
    output_recipients: [SpkEntry; MAX_OUTPUTS_COUNT],
}

pub const MINT_PAYLOAD_LEN: usize = size_of::<MintPayloadLayout>();
pub const TRANSFER_PAYLOAD_LEN: usize = size_of::<TransferPayloadLayout>();

// consts for re-use in covenants
impl MintPayloadHeader {
    pub const MAGIC: ByteRange = ByteRange::new(offset_of!(MintPayloadHeader, magic), 6);

    pub const ASSET_ID: ByteRange = ByteRange::new(offset_of!(MintPayloadHeader, asset_id), ASSET_ID_SIZE);

    pub const AUTHORITY_SPK: SpkRanges = spk_ranges(offset_of!(MintPayloadHeader, authority_spk));

    pub const TOKEN_SPK: SpkRanges = spk_ranges(offset_of!(MintPayloadHeader, token_spk));

    pub const REMAINING_SUPPLY: ByteRange = ByteRange::new(offset_of!(MintPayloadHeader, remaining_supply), 8);

    pub const OP: ByteRange = ByteRange::new(offset_of!(MintPayloadHeader, op), 1);

    pub const TOTAL_AMOUNT: ByteRange = ByteRange::new(offset_of!(MintPayloadHeader, total_amount), 8);
}

impl TransferPayloadHeader {
    pub const MAGIC: ByteRange = ByteRange::new(offset_of!(TransferPayloadHeader, magic), 6);

    pub const ASSET_ID: ByteRange = ByteRange::new(offset_of!(TransferPayloadHeader, asset_id), ASSET_ID_SIZE);

    pub const AUTHORITY_SPK: SpkRanges = spk_ranges(offset_of!(TransferPayloadHeader, authority_spk));

    pub const TOKEN_SPK: SpkRanges = spk_ranges(offset_of!(TransferPayloadHeader, token_spk));

    pub const OP: ByteRange = ByteRange::new(offset_of!(TransferPayloadHeader, op), 1);

    pub const TOTAL_AMOUNT: ByteRange = ByteRange::new(offset_of!(TransferPayloadHeader, total_amount), 8);
}

impl MintPayloadLayout {
    pub const OUTPUT0_AMOUNT: ByteRange = ByteRange::new(offset_of!(MintPayloadLayout, output0_amount), 8);

    pub const OUTPUT0_RECIPIENT: SpkRanges = spk_ranges(offset_of!(MintPayloadLayout, output0_recipient));
}

impl TransferPayloadLayout {
    pub const INPUT_AMOUNTS_BASE: usize = offset_of!(TransferPayloadLayout, input_amounts);
    pub const OUTPUT_AMOUNTS_BASE: usize = offset_of!(TransferPayloadLayout, output_amounts);
    pub const OUTPUT_RECIPIENTS_BASE: usize = offset_of!(TransferPayloadLayout, output_recipients);

    pub const fn input_amount(i: usize) -> ByteRange {
        // i must be < MAX_INPUTS_COUNT
        ByteRange::new(Self::INPUT_AMOUNTS_BASE + i * 8, 8)
    }
    pub const fn output_amount(i: usize) -> ByteRange {
        ByteRange::new(Self::OUTPUT_AMOUNTS_BASE + i * 8, 8)
    }
    pub const fn output_recipient(i: usize) -> SpkRanges {
        spk_ranges(Self::OUTPUT_RECIPIENTS_BASE + i * size_of::<SpkEntry>())
    }
}

// byte rang utils
#[derive(Clone, Copy)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub const fn new(start: usize, len: usize) -> Self {
        Self { start, end: start + len }
    }
}

#[derive(Clone, Copy)]
pub struct SpkRanges {
    pub len: ByteRange,   // 1 byte
    pub bytes: ByteRange, // SPK_BYTES_MAX bytes
}

pub const fn spk_ranges(base: usize) -> SpkRanges {
    let len_off = offset_of!(SpkEntry, len);
    let bytes_off = offset_of!(SpkEntry, bytes);
    SpkRanges { len: ByteRange::new(base + len_off, 1), bytes: ByteRange::new(base + bytes_off, SPK_BYTES_MAX) }
}
