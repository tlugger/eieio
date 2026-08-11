//! Generating what SDK §1 promises: the ABI exports, the port enums, the `Prop<T>`
//! initializers, and the `eio:manifest` custom section.
//!
//! # The shape of the generated code
//!
//! One `const _: () = { .. }` block holding everything except the port enums and the
//! manifest static. Scoping it that way keeps the generated names out of the block
//! author's namespace — a block with a function called `state` should keep it — while the
//! `#[unsafe(no_mangle)]` exports still reach the module's export table, because a
//! `no_mangle` symbol is not scoped by the module it was written in.
//!
//! # Why the exports are generated rather than written
//!
//! ABI §4.1 makes eight exports REQUIRED and §4.2 makes three more conditional on declared
//! capabilities. Every one of them is `(ptr, len) -> i32` plumbing over a safe method, and
//! every one has to get ABI §6.1's ownership rule right: the host allocates an inbound
//! payload, the guest owns it from the moment the call begins, and the *guest* frees it.
//! That is the single most repeated piece of unsafe-adjacent bookkeeping in the ecosystem,
//! and it is exactly what a macro should own so that no block author writes it twice.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use eio_manifest::{Abi, Manifest, Port, Property, PropertyType};

use crate::parse::Block;

/// Expands a parsed `#[block]` into the item plus everything the ABI needs.
pub fn expand(block: &Block) -> TokenStream {
    // `#[prop]` is this macro's input, not an attribute the compiler knows. An attribute
    // macro re-emits the item it was given, so the field attributes have to be stripped
    // here or the struct comes back referring to an attribute that does not exist.
    let mut item = block.item.clone();
    if let syn::Fields::Named(fields) = &mut item.fields {
        for field in &mut fields.named {
            field.attrs.retain(|attr| !attr.path().is_ident("prop"));
        }
    }
    let item = &item;
    let ident = &item.ident;

    let ports_in = port_enum(&format_ident!("In"), &block.inputs, "input");
    let ports_out = port_enum(&format_ident!("Out"), &block.outputs, "output");
    let manifest = manifest_section(block);
    let exports = exports(block);
    let state = state(block);
    let declared = declared_types(block);
    let capabilities = capability_trait(block);
    let builder = build_expression(block);

    quote! {
        #item
        #ports_in
        #ports_out
        #manifest

        const _: () = {
            #state
            #exports
        };

        // The same constructor the generated exports use, reachable by name so a test can
        // build the block without re-deriving its `prop_id`s (SDK §6.1).
        //
        // Off the guest, where nothing calls it: a block's exports use the `build()` above,
        // and this would be a second copy of it riding into every module on the hope that
        // `--gc-sections` removes it. The size-optimization defaults are still an open item
        // (SDK §8), so leaning on that would be leaning on something unpinned.
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        impl ::eio_sdk::Bound for #ident {
            fn bound() -> #ident {
                #builder
            }
        }

        // A block that forgot `impl Block for _` should hear about the trait, not about
        // twenty missing methods inside generated code it never wrote.
        const _: fn() = || {
            fn implements<T: ::eio_sdk::Block>() {}
            implements::<#ident>();
        };

        #declared
        #capabilities
    }
}

/// The capability accessors, as an extension trait carrying only what the block declared
/// (SDK §3).
///
/// `Ctx` is one type in `eio-sdk` and cannot conditionally have methods, so the gate is a
/// trait generated per block: an undeclared `ctx.gpio()` is then a missing method, which is
/// the compile error SDK §3 asks for rather than an `ERR_CAPABILITY` a deployer finds.
///
/// Emitted only when something was declared. A block with `capabilities()` gets no trait at
/// all, which is the same thing said with less code — and keeps `cargo expand` output for
/// the common case free of an empty trait nobody implements.
fn capability_trait(block: &Block) -> Option<TokenStream> {
    if block.capabilities.is_empty() {
        return None;
    }

    let accessors: Vec<_> = block
        .capabilities
        .iter()
        .map(|capability| {
            let (method, ty, namespace) = match capability.value {
                eio_manifest::Capability::State => ("state", quote!(State), "eio:state"),
                eio_manifest::Capability::Timer => ("timers", quote!(Timer), "eio:timer"),
                eio_manifest::Capability::Gpio => ("gpio", quote!(Gpio), "eio:gpio"),
                eio_manifest::Capability::I2c => ("i2c", quote!(I2c), "eio:i2c"),
                eio_manifest::Capability::Http => ("http", quote!(Http), "eio:http"),
            };
            let method = format_ident!("{method}");
            let doc = format!("The `{namespace}` capability (SDK §3).");
            (
                quote! {
                    #[doc = #doc]
                    fn #method(&mut self) -> ::eio_sdk::#ty<'_>;
                },
                quote! {
                    fn #method(&mut self) -> ::eio_sdk::#ty<'_> {
                        ::eio_sdk::#ty { _ctx: ::core::marker::PhantomData }
                    }
                },
            )
        })
        .collect();

    let declarations = accessors.iter().map(|(declaration, _)| declaration);
    let definitions = accessors.iter().map(|(_, definition)| definition);

    Some(quote! {
        /// The capabilities this block declared (SDK §3).
        ///
        /// In scope wherever the block is written, so `ctx.state()` resolves. A capability
        /// the block did not declare has no method here and does not compile.
        #[allow(dead_code)]
        pub trait Capabilities {
            #(#declarations)*
        }

        impl Capabilities for ::eio_sdk::Ctx {
            #(#definitions)*
        }
    })
}

/// Checks each `#[prop]` field's Rust type against the `ty = ".."` it declares (SDK §1.2).
///
/// This is the property half of SDK §1's "unrepresentable rather than merely validated".
/// The manifest says `"type":"int"` and the field says `Prop<f64>`; without this the two
/// disagree silently until a host evaluates the property, type-checks it against the
/// manifest, encodes an int, and the guest fails to decode it — at run time, per signal,
/// on a deployed node. Here it is a compile error naming the field.
fn declared_types(block: &Block) -> TokenStream {
    let checks: Vec<_> = block
        .props
        .iter()
        .map(|prop| {
            let ty = &prop.ty;
            let marker = match prop.declared {
                PropertyType::Bool => quote! { ::eio_sdk::ty::Bool },
                PropertyType::Int => quote! { ::eio_sdk::ty::Int },
                PropertyType::Float => quote! { ::eio_sdk::ty::Float },
                PropertyType::String => quote! { ::eio_sdk::ty::Str },
                PropertyType::Bytes => quote! { ::eio_sdk::ty::Bytes },
                PropertyType::Any => quote! { ::eio_sdk::ty::Any },
            };
            // Spanned at the field, so the error underlines the declaration that is wrong
            // rather than the `#[block]` attribute at the top of the struct.
            quote::quote_spanned! { prop.span =>
                const _: fn() = || {
                    fn declared_as<D, P: ::eio_sdk::PropDeclared<D>>() {}
                    declared_as::<#marker, #ty>();
                };
            }
        })
        .collect();
    quote! { #(#checks)* }
}

/// The `In`/`Out` enum whose discriminants are ABI §5.2's port indices.
///
/// A `u32` discriminant per variant, in declaration order, so the enum *is* the index
/// mapping rather than something kept in step with it. `Out::index()` is what `Ctx::emit`
/// receives, which is what makes emitting to an undeclared port a compile error: there is
/// no variant to name.
fn port_enum(name: &syn::Ident, ports: &[crate::parse::Port], label: &str) -> TokenStream {
    // One pass. The three renderings — the variant, its index, its name — are the same
    // fact three times, and building them together is what keeps them from drifting.
    let mut variants = Vec::with_capacity(ports.len());
    let mut indices = Vec::with_capacity(ports.len());
    let mut arms = Vec::with_capacity(ports.len());
    for (index, port) in ports.iter().enumerate() {
        let variant = &port.variant;
        let index = index as u32;
        let text = &port.name;
        let doc = format!("`{}` — {label} port {index}.", port.name);
        variants.push(quote! { #[doc = #doc] #variant = #index });
        indices.push(quote! { #name::#variant => #index });
        arms.push(quote! { #name::#variant => #text });
    }

    let doc = format!(
        "The block's {label} ports (ABI §5.2). The discriminant is the port index, so a \
         port this block does not declare cannot be named."
    );

    // An empty enum is uninhabited, which is the honest type for a block with no ports of
    // that direction: there is no value to construct and no call to make. `#[repr(u32)]` is
    // rejected on a zero-variant enum, so it goes on only when there is a discriminant for
    // it to describe.
    let repr = (!ports.is_empty()).then(|| quote! { #[repr(u32)] });

    // Only outputs are emitted on, so only `Out` converts. Giving `In` the same conversion
    // would let `ctx.emit(In::Default, ..)` compile, which is the mistake the typed enums
    // exist to make unwriteable.
    let into_sdk = (name == "Out").then(|| {
        quote! {
            impl ::core::convert::From<#name> for ::eio_sdk::Out {
                fn from(port: #name) -> ::eio_sdk::Out {
                    ::eio_sdk::Out::new(port.index())
                }
            }
        }
    });
    quote! {
        #[doc = #doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #repr
        #[allow(dead_code)]
        pub enum #name { #(#variants),* }

        #[allow(dead_code)]
        impl #name {
            /// The port index this variant is (ABI §5.2).
            pub const fn index(self) -> u32 {
                match self { #(#indices),* }
            }

            /// The port's declared name.
            pub const fn name(self) -> &'static str {
                match self { #(#arms),* }
            }
        }

        #into_sdk
    }
}

/// The `eio:manifest` custom section (ABI §4.4).
///
/// Built at expansion time from the same attributes the exports come from, which is what
/// makes a manifest/module mismatch unrepresentable rather than merely validated: there is
/// one description of the block and both readings derive from it.
///
/// `#[used]` matters. Nothing references this static, so without it the linker is free to
/// drop the symbol and the section with it — leaving a module that passes every test here
/// and is refused as undescribed by a host.
fn manifest_section(block: &Block) -> TokenStream {
    let json = manifest(block).to_json();
    let len = json.len();
    let bytes = proc_macro2::Literal::byte_string(json.as_bytes());
    let doc = format!(
        "The block's ABI §11 manifest, as ABI §4.4's custom section:\n\n```json\n{json}\n```"
    );

    quote! {
        // Gated to the guest, because `link_section` is a *target* concept: Mach-O section
        // specifiers are `"segment,section"` and reject this name outright, so a host build
        // of the same source would not compile. The bytes are identical either way, which
        // is what lets a native test read exactly what the module carries.
        #[doc = #doc]
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        #[used]
        #[unsafe(link_section = "eio:manifest")]
        static EIO_MANIFEST: [u8; #len] = *#bytes;

        #[doc = #doc]
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        #[allow(dead_code)]
        static EIO_MANIFEST: [u8; #len] = *#bytes;
    }
}

/// The manifest, built as `eio-manifest`'s own type and serialized by it.
///
/// Not hand-written JSON. ABI §11.1 has one implementation (DAEMON §1) and this is it, so
/// the document a host will parse is produced by the crate that will parse it — the field
/// order, the omissions (`required: false` and an absent `default` are skipped, and ABI
/// §11.1 says `null` is not a spelling of absence), and the escaping are all that crate's,
/// not a second opinion about them.
///
/// Free here in a way it would not be inside `eio-sdk`: a proc-macro crate is compiled for
/// the host and run by the compiler, so none of this reaches a guest.
fn manifest(block: &Block) -> Manifest {
    Manifest {
        name: block.name.clone(),
        version: block.version.clone(),
        abi: Abi::CURRENT,
        description: block.description.clone().unwrap_or_default(),
        capabilities: block
            .capabilities
            .iter()
            .map(|capability| capability.value)
            .collect(),
        inputs: block
            .inputs
            .iter()
            .map(|port| Port {
                name: port.name.clone(),
            })
            .collect(),
        outputs: block
            .outputs
            .iter()
            .map(|port| Port {
                name: port.name.clone(),
            })
            .collect(),
        properties: block
            .props
            .iter()
            .map(|prop| Property {
                name: prop.name.clone(),
                ty: prop.declared,
                description: prop.description.clone().unwrap_or_default(),
                default: prop.default.clone(),
                required: prop.required,
            })
            .collect(),
        targets: vec![String::from(eio_manifest::PORTABLE_TARGET)],
        aot: Vec::new(),
    }
}

/// A fresh block with every `Prop<T>` bound to its `prop_id` (ABI §5.2).
///
/// Shared by the generated `build()` and the `Bound` impl, so a test and the runtime
/// construct the instance the same way — there is one definition of which field is
/// `prop_id` 0.
fn build_expression(block: &Block) -> TokenStream {
    let ident = &block.item.ident;
    let initializers: Vec<_> = block
        .props
        .iter()
        .enumerate()
        .map(|(index, prop)| {
            let field = &prop.ident;
            let index = index as u32;
            quote! { #field: ::eio_sdk::Prop::new(::eio_sdk::PropId::new(#index)) }
        })
        .collect();
    let others: Vec<_> = block
        .item
        .fields
        .iter()
        .filter(|field| !field.attrs.iter().any(|attr| attr.path().is_ident("prop")))
        .map(|field| {
            let name = field.ident.as_ref().expect("named fields");
            quote! { #name: ::core::default::Default::default() }
        })
        .collect();
    quote! { #ident { #(#initializers,)* #(#others,)* } }
}

/// The instance's state: the block, its `Ctx`, and its descriptor.
///
/// One `static mut` behind accessors, which is what a single-threaded WASM instance is:
/// ABI §1.2 gives it one caller at a time and forbids the host from calling into a guest
/// that is mid-call, so there is no concurrency for a lock to protect and no atomics on
/// the leaf tier to build one from.
fn state(block: &Block) -> TokenStream {
    let ident = &block.item.ident;
    let builder = build_expression(block);
    quote! {
        /// The instance, live between `eio_configure` and death.
        static mut BLOCK: ::core::option::Option<#ident> = ::core::option::Option::None;
        /// The host channel, built from the descriptor's limits (ABI §5.2).
        static mut CTX: ::core::option::Option<::eio_sdk::Ctx> = ::core::option::Option::None;

        /// The block and its context, or `None` before `eio_configure`.
        ///
        /// # Safety
        ///
        /// ABI §1.2: the host serializes every call into an instance and MUST NOT call a
        /// guest that is mid-call, so only one `&mut` to these can exist at a time. The
        /// callback that holds it always returns before the next begins.
        unsafe fn live() -> ::core::option::Option<(&'static mut #ident, &'static mut ::eio_sdk::Ctx)> {
            // SAFETY: ABI §1.2's single-threaded actor model, as above. Taking the two
            // references together is what keeps them from being taken separately and
            // overlapping.
            unsafe {
                let block = (&raw mut BLOCK).as_mut()?.as_mut()?;
                let ctx = (&raw mut CTX).as_mut()?.as_mut()?;
                ::core::option::Option::Some((block, ctx))
            }
        }

        /// A fresh block with every `Prop<T>` bound to its `prop_id` (ABI §5.2).
        fn build() -> #ident {
            #builder
        }
    }
}

/// Every ABI §4.1 export, plus the §4.2 callbacks the declared capabilities require.
fn exports(block: &Block) -> TokenStream {
    let required = required_exports();
    let optional = optional_exports(block);
    quote! { #required #optional }
}

fn required_exports() -> TokenStream {
    quote! {
        #[unsafe(no_mangle)]
        pub extern "C" fn eio_abi_version() -> i32 {
            // ABI §12: `(major << 16) | minor`. This SDK implements 1.0, and the manifest
            // section above says the same.
            0x0001_0000
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn eio_configure(ptr: i32, len: i32) -> i32 {
            // ABI §6.1 and §9.2: the host allocated this and the guest owns it from the
            // moment the call began, so the guest frees it. `take` copies and frees in one
            // step, so no path below can skip the free.
            ::eio_sdk::logger::init();
            let decoded = ::eio_sdk::runtime::take_with(ptr, len, ::eio_sdk::Descriptor::from_cbor);

            let descriptor = match decoded {
                ::core::result::Result::Ok(descriptor) => descriptor,
                ::core::result::Result::Err(error) => return ::eio_sdk::runtime::refuse(&error),
            };

            let mut ctx = ::eio_sdk::Ctx::new(descriptor.limits);
            let mut block = build();
            // Sequenced, not nested: `finish(&mut ctx, configure(.., &mut ctx, ..))`
            // takes the first borrow before evaluating the argument that needs the second.
            let result = ::eio_sdk::Block::configure(&mut block, &mut ctx, &descriptor);
            let status = ::eio_sdk::runtime::finish(&mut ctx, result);
            if status == 0 {
                // SAFETY: ABI §5.1 — `eio_configure` runs once and before every other
                // callback, and ABI §1.2 gives the instance one caller at a time, so
                // nothing else holds a reference to either static here.
                unsafe {
                    BLOCK = ::core::option::Option::Some(block);
                    CTX = ::core::option::Option::Some(ctx);
                }
            }
            status
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn eio_start() -> i32 {
            // SAFETY: ABI §1.2 — one caller at a time, and the host MUST NOT call into a
            // guest that is mid-call, so this borrow cannot overlap another. It is dropped
            // before this function returns, and the next callback takes a fresh one.
            ::eio_sdk::runtime::dispatch(unsafe { live() }, ::eio_sdk::Block::start)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn eio_stop() -> i32 {
            // SAFETY: as `eio_start` — ABI §1.2's serialized callbacks.
            ::eio_sdk::runtime::dispatch(unsafe { live() }, ::eio_sdk::Block::stop)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn eio_process_signals(input_port: i32, ptr: i32, len: i32) -> i32 {
            // The batch is decoded while the host's buffer is still borrowed, and the
            // buffer is released before the block's callback runs — so the raw payload is
            // not held across a callback that may allocate (ABI §9.5).
            let decoded = ::eio_sdk::runtime::take_with(ptr, len, ::eio_sdk::runtime::decode);
            // SAFETY: as `eio_start` — ABI §1.2's serialized callbacks.
            ::eio_sdk::runtime::dispatch(unsafe { live() }, move |block, ctx| {
                ::eio_sdk::Block::process_signals(block, ctx, input_port as u32, decoded?)
            })
        }
    }
}

/// ABI §4.2's callbacks. Present only when the paired capability is declared — the pairing
/// is required in both directions, and an export the host can never invoke is a block that
/// believes it holds a capability it never asked for.
fn optional_exports(block: &Block) -> TokenStream {
    let declared = |wanted: eio_manifest::Capability| {
        block
            .capabilities
            .iter()
            .any(|capability| capability.value == wanted)
    };

    let timer = declared(eio_manifest::Capability::Timer).then(|| {
        quote! {
            #[unsafe(no_mangle)]
            pub extern "C" fn eio_on_timer(timer_id: i32) -> i32 {
                // SAFETY: ABI §1.2 — timer callbacks are serialized with every other
                // callback, so this borrow cannot overlap one.
                ::eio_sdk::runtime::dispatch(unsafe { live() }, move |block, ctx| {
                    ::eio_sdk::Block::on_timer(block, ctx, timer_id as u32)
                })
            }
        }
    });

    let gpio = declared(eio_manifest::Capability::Gpio).then(|| {
        quote! {
            #[unsafe(no_mangle)]
            pub extern "C" fn eio_on_gpio(watch_id: i32, value: i32) -> i32 {
                // SAFETY: ABI §1.2 — serialized with every other callback.
                ::eio_sdk::runtime::dispatch(unsafe { live() }, move |block, ctx| {
                    ::eio_sdk::Block::on_gpio(block, ctx, watch_id as u32, value)
                })
            }
        }
    });

    let http = declared(eio_manifest::Capability::Http).then(|| {
        quote! {
            #[unsafe(no_mangle)]
            pub extern "C" fn eio_on_http(req_id: i32, status: i32, ptr: i32, len: i32) -> i32 {
                // ABI §7.6: the response is host-allocated through `eio_alloc` and the
                // guest MUST free it — the same ownership rule as an inbound batch (§9.2).
                // Borrowed across the callback, which ABI §6.1 permits ("before or after
                // returning"), so the body reaches the block without a copy.
                ::eio_sdk::runtime::take_with(ptr, len, |body| {
                    // SAFETY: ABI §1.2 — serialized with every other callback.
                    ::eio_sdk::runtime::dispatch(unsafe { live() }, move |block, ctx| {
                        ::eio_sdk::Block::on_http(block, ctx, req_id as u32, status, body)
                    })
                })
            }
        }
    });

    quote! { #timer #gpio #http }
}
