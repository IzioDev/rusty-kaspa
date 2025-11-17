pub mod v8 {
    include!(concat!(env!("OUT_DIR"), "/protowire.rs"));
}

#[cfg(test)]
mod tests;
