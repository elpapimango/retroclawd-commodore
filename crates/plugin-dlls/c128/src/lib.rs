//! `c128_machine_plugin` as a loadable library: the entry points `retroclawd` looks for,
//! forwarding to the implementation in the `c128-machine-plugin` crate.

plugin_api::export_plugin!(c128_machine_plugin_impl::entry_v1);
