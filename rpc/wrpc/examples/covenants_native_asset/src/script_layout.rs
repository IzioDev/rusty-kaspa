use core::mem::{offset_of, size_of};

pub const SPK_VERSION_SIZE: usize = 2;
// spk can be either 36 | 37
pub const SPK_BYTES_MIN: usize = 36;
pub const SPK_BYTES_MAX: usize = 37;
pub const SPK_FIELD_LEN: usize = 1 + SPK_BYTES_MAX;

pub const AMOUNT_LEN: usize = 8;

#[repr(C)]
struct StatePrefixLayout {
    op_data_amount: u8,
    amount: [u8; AMOUNT_LEN],
    op_data_spk_field: u8,
    spk_field: [u8; SPK_FIELD_LEN],
}

#[repr(C)]
struct CovenantStateLayout {
    version: [u8; SPK_VERSION_SIZE],
    prefix: StatePrefixLayout,
}

// Script layout (script bytes, not including covenant spk version):
// [OpData32][covenant_id 32]
// [OpData8][amount 8]
// [OpData38][spk_field 38] - includes spk version
pub const STATE_PREFIX_LEN: usize = size_of::<StatePrefixLayout>();

pub const AMOUNT_OFFSET: usize = offset_of!(StatePrefixLayout, amount);
pub const SPK_FIELD_OFFSET: usize = offset_of!(StatePrefixLayout, spk_field);
pub const LOGIC_TAIL_OFFSET: usize = STATE_PREFIX_LEN;

const COVENANT_PREFIX_OFFSET: usize = offset_of!(CovenantStateLayout, prefix);
pub const SPK_AMOUNT_OFFSET: usize = COVENANT_PREFIX_OFFSET + AMOUNT_OFFSET;
// enhancement: spk_spk_field is confusing, but actually refers to spk_field (token = holder, minter = authorithy) of the spk (covenant script)
pub const SPK_SPK_FIELD_OFFSET: usize = COVENANT_PREFIX_OFFSET + SPK_FIELD_OFFSET;
pub const SPK_LOGIC_TAIL_OFFSET: usize = COVENANT_PREFIX_OFFSET + LOGIC_TAIL_OFFSET;

pub const SPK_FIELD_LEN_OFFSET: usize = SPK_SPK_FIELD_OFFSET;

// pub const SPK_FIELD_BYTES_OFFSET: usize = SPK_SPK_FIELD_OFFSET + 1;
// pub const SPK_FIELD_BYTES_END: usize = SPK_FIELD_BYTES_OFFSET + SPK_BYTES_MAX;
