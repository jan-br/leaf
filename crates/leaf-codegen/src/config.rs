//! The `#[value]` / `#[config_properties]` / `derive(BindTarget)` / `#[converter]`
//! codegen (binding-conversion phase3/07: config-metadata + binder + converters).
//!
//! This is the heavy, unit-testable lowering the THIN config macros call. It owns
//! the binding half of the four-layer pipeline:
//!
//! 1. **`derive(BindTarget)`** — lower a `struct` to the const
//!    `::leaf_core::NodeSchema` + the cursor-calling `::leaf_core::BindTarget::bind`
//!    impl (the JavaBean field-fold), via ABSOLUTE `::leaf_core` paths. A scalar
//!    field binds through `cursor.scalar::<T>(name)`; a nested `BindTarget` field
//!    binds through `cursor.nested`; a `Vec<T>` through `cursor.list`.
//! 2. **`#[config_properties(prefix = "app")]`** — emit the `derive(BindTarget)`
//!    artifact PLUS one `::leaf_core::ConfigMetadataRow` into the `CONFIG_METADATA`
//!    slice (the anti-DCE/config-doc anchor) and one const `::leaf_core::ConfigGroup`
//!    documenting the bound keys (the `leaf metadata` rollup input).
//! 3. **`#[value("${k:def}")]`** — lower a value template to the const
//!    `&[::leaf_core::ValueSegment]` the placeholder engine interprets (delegating
//!    to the already-built [`crate::parsers`] splitter).
//! 4. **`#[converter]`** — register a user `Converter` impl into the `CATALOGS`
//!    slice via one const `::leaf_core::CatalogRow`.
//!
//! Every emitted const is absolute-`::leaf_core`-pathed (the thin-macro rule,
//! charter §2.10). The bind schema is derived ENTIRELY here so the runtime sees one
//! const `NodeSchema` + a monomorphized `bind` — no reflection, no runtime schema
//! construction.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Type};

use crate::descriptor::EmitError;

// ─────────────────────────── derive(BindTarget) ─────────────────────────────

/// One bindable field lowered from a `struct` field.
#[derive(Clone, Debug)]
struct BindField {
    /// The Rust field ident (the `bind` fold's local).
    ident: syn::Ident,
    /// The canonical kebab name (the relaxed-binding key the cursor reads).
    canonical: String,
    /// The field type (drives the scalar/nested/list cursor call).
    ty: Type,
    /// The field's binding shape.
    shape: FieldShape,
}

/// The binding shape of a field — which `BindCursor` helper lowers it + which
/// `NodeSchema` node documents it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FieldShape {
    /// A leaf scalar coerced via `FromConfigValue` (`cursor.scalar`).
    Scalar,
    /// A homogeneous list `Vec<T>` (`cursor.list`).
    List,
    /// A nested `BindTarget` object (`cursor.nested`).
    Nested,
}

/// Derive the [`leaf_core::BindTarget`] artifact for a `struct`: the const
/// `NodeSchema` (an `Object` of one `Field` per struct field) and the
/// cursor-calling `bind` impl that folds every field nearest-wins. Both via
/// absolute `::leaf_core` paths.
///
/// # Errors
/// [`EmitError`] when the target is not a named-field struct (a tuple/unit struct
/// or an enum has no canonical bindable field set) or is generic (a generic bind
/// target has no single concrete schema).
pub fn emit_bind_target(input: &DeriveInput) -> Result<TokenStream, EmitError> {
    let ident = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{ident}` is a generic #[derive(BindTarget)]: a generic config target has no \
                 single concrete bind schema. Bind a concrete instantiation."
            ),
        });
    }
    let fields = struct_fields(input)?;

    let schema_const = emit_schema(ident, &fields);
    let bind_impl = emit_bind_impl(ident, &fields);

    Ok(quote! {
        #schema_const
        #bind_impl
    })
}

/// Read a named-field struct's fields into the binding model (the one place the
/// derive AST is read for binding).
fn struct_fields(input: &DeriveInput) -> Result<Vec<BindField>, EmitError> {
    let Data::Struct(data) = &input.data else {
        return Err(EmitError {
            message: format!(
                "`{}` is not a struct: #[derive(BindTarget)] / #[config_properties] target a \
                 named-field struct (the JavaBean shape).",
                input.ident
            ),
        });
    };
    let Fields::Named(named) = &data.fields else {
        return Err(EmitError {
            message: format!(
                "`{}` has no named fields: a bind target must be a named-field struct.",
                input.ident
            ),
        });
    };
    let mut out = Vec::new();
    for f in &named.named {
        let ident = f.ident.clone().expect("a named field has an ident");
        let canonical = canonical_name(&ident.to_string());
        let shape = field_shape(&f.ty);
        out.push(BindField { ident, canonical, ty: f.ty.clone(), shape });
    }
    Ok(out)
}

/// Classify a field's binding shape from its type: `Vec<T>` → `List`; a primitive
/// scalar / `String` / common leaf → `Scalar`; anything else → `Nested` (a
/// `BindTarget` object). This is a conservative structural classification; the
/// monomorphized `bind` call still type-checks against the real trait bound.
fn field_shape(ty: &Type) -> FieldShape {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        let name = seg.ident.to_string();
        if name == "Vec" {
            return FieldShape::List;
        }
        if is_scalar_ident(&name) {
            return FieldShape::Scalar;
        }
        return FieldShape::Nested;
    }
    // A non-path type (reference, array, …) is treated as a scalar leaf.
    FieldShape::Scalar
}

/// Whether a leading type ident names a built-in scalar leaf (the
/// `FromConfigValue` grammar set). Anything else is a nested object.
fn is_scalar_ident(name: &str) -> bool {
    matches!(
        name,
        "String"
            | "bool"
            | "char"
            | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
            | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "f32" | "f64"
            | "Duration" | "Period" | "DataSize"
            | "PathBuf"
            | "IpAddr" | "SocketAddr"
    )
}

/// Emit the const `NodeSchema` for the struct (one `Field` per struct field).
fn emit_schema(ident: &syn::Ident, fields: &[BindField]) -> TokenStream {
    let mangled = mangle(&ident.to_string());
    let fields_ident = format_ident!("__LEAF_BIND_FIELDS_{}", mangled);
    let schema_ident = format_ident!("__LEAF_BIND_SCHEMA_{}", mangled);

    let field_rows = fields.iter().map(|f| {
        let canonical = &f.canonical;
        let node = field_node_schema(f);
        // A field with a Default-derivable type is treated as "has a default" so
        // absence is Unbound, not an error (the JavaBean default-fill convention).
        quote! {
            ::leaf_core::Field {
                canonical: #canonical,
                schema: #node,
                has_default: true,
            }
        }
    });

    quote! {
        #[allow(non_upper_case_globals)]
        static #fields_ident: &[::leaf_core::Field] = &[ #(#field_rows),* ];
        #[allow(non_upper_case_globals)]
        static #schema_ident: ::leaf_core::NodeSchema = ::leaf_core::NodeSchema::Object {
            method: ::leaf_core::BindMethod::JavaBean,
            fields: #fields_ident,
        };
    }
}

/// The `&'static NodeSchema` reference a `Field` points at, by shape. A scalar is
/// the shared `&::leaf_core::NodeSchema::Scalar` const; a list wraps the element
/// scalar schema; a nested object references the inner type's derived `SCHEMA`.
fn field_node_schema(f: &BindField) -> TokenStream {
    match f.shape {
        FieldShape::Scalar => quote! { &::leaf_core::NodeSchema::Scalar },
        FieldShape::List => quote! { &::leaf_core::NodeSchema::List(&::leaf_core::NodeSchema::Scalar) },
        FieldShape::Nested => {
            let ty = &f.ty;
            quote! { <#ty as ::leaf_core::BindTarget>::SCHEMA }
        }
    }
}

/// Emit the `impl ::leaf_core::BindTarget` block: the `const SCHEMA` pointer + the
/// cursor-calling `bind` fold.
fn emit_bind_impl(ident: &syn::Ident, fields: &[BindField]) -> TokenStream {
    let mangled = mangle(&ident.to_string());
    let schema_ident = format_ident!("__LEAF_BIND_SCHEMA_{}", mangled);

    let binds = fields.iter().map(|f| {
        let fid = &f.ident;
        let canonical = &f.canonical;
        let ty = &f.ty;
        let call = match f.shape {
            FieldShape::Scalar => quote! { __cursor.scalar::<#ty>(#canonical) },
            FieldShape::List => {
                let elem = vec_elem(ty);
                quote! { __cursor.list::<#elem>(#canonical) }
            }
            FieldShape::Nested => quote! { __cursor.nested::<#ty>(#canonical) },
        };
        quote! {
            match #call {
                ::leaf_core::BindResult::Bound(__v) => {
                    __out.#fid = __v;
                    __any = true;
                }
                ::leaf_core::BindResult::Unbound => {}
                ::leaf_core::BindResult::Failed(__e) => {
                    return ::leaf_core::BindResult::Failed(__e);
                }
            }
        }
    });

    quote! {
        impl ::leaf_core::BindTarget for #ident {
            const SCHEMA: &'static ::leaf_core::NodeSchema = &#schema_ident;
            fn bind(
                __cursor: &mut ::leaf_core::BindCursor<'_, '_>,
            ) -> ::leaf_core::BindResult<Self> {
                let mut __out = <#ident as ::core::default::Default>::default();
                let mut __any = false;
                #(#binds)*
                if __any {
                    ::leaf_core::BindResult::Bound(__out)
                } else {
                    ::leaf_core::BindResult::Unbound
                }
            }
        }
    }
}

/// The element type `T` of a `Vec<T>` (or the type itself if not a `Vec`).
fn vec_elem(ty: &Type) -> Type {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Vec"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return inner.clone();
    }
    ty.clone()
}

// ─────────────────────── #[config_properties(prefix=...)] ────────────────────

/// The parsed `#[config_properties(prefix = "app")]` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigPropertiesArgs {
    /// The canonical config-key prefix this bean binds under (required).
    pub prefix: String,
    /// Whether the bean opts into post-bind JSR validation: a bare `validate` flag
    /// makes the C2 bind thunk run `::leaf_validation::validate_config` over the bound
    /// value (the bean must `#[derive(Validate)]` / `impl ValidateInto`) and surface
    /// the aggregated `ValidationError` as a bind fault. Off by default — an existing
    /// non-`Validate` config bean is unaffected.
    pub validate: bool,
}

/// Parse the `#[config_properties(prefix = "app")]` attribute body.
///
/// # Errors
/// [`EmitError`] when `prefix` is missing, not a string, or an unknown key appears.
pub fn parse_config_args(attr: TokenStream) -> Result<ConfigPropertiesArgs, EmitError> {
    let parser =
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
    let metas = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[config_properties] arguments: {e}"),
    })?;
    let mut prefix = None;
    let mut validate = false;
    for meta in metas {
        match meta {
            // The `prefix = "..."` key-value pair (required).
            syn::Meta::NameValue(nv) => {
                let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
                match key.as_str() {
                    "prefix" => {
                        prefix = Some(str_value(&nv.value).ok_or_else(|| EmitError {
                            message: "`prefix` must be a string literal".into(),
                        })?);
                    }
                    other => {
                        return Err(EmitError {
                            message: format!(
                                "unknown #[config_properties] argument `{other}` (expected `prefix` / `validate`)"
                            ),
                        });
                    }
                }
            }
            // The bare-path `validate` opt-in flag.
            syn::Meta::Path(path) => {
                let key = path.get_ident().map(ToString::to_string).unwrap_or_default();
                match key.as_str() {
                    "validate" => validate = true,
                    other => {
                        return Err(EmitError {
                            message: format!(
                                "unknown #[config_properties] argument `{other}` (expected `prefix` / `validate`)"
                            ),
                        });
                    }
                }
            }
            syn::Meta::List(_) => {
                return Err(EmitError {
                    message: "#[config_properties] arguments are `prefix = \"...\"` and the bare \
                              `validate` flag"
                        .into(),
                });
            }
        }
    }
    let prefix = prefix.ok_or_else(|| EmitError {
        message: "#[config_properties] requires a `prefix = \"...\"` argument".into(),
    })?;
    Ok(ConfigPropertiesArgs { prefix, validate })
}

/// Emit the full `#[config_properties(prefix = "app")]` artifact: the
/// `derive(BindTarget)` schema + impl, one `::leaf_core::ConfigMetadataRow`
/// anti-DCE anchor into the `CONFIG_METADATA` slice, and one const
/// `::leaf_core::ConfigGroup` documenting the bound keys.
///
/// The contract identity is module-qualified at the DEFINITION SITE (a thin macro
/// cannot resolve the bean's module at expansion), exactly like the component
/// emitter.
///
/// # Errors
/// [`EmitError`] per [`emit_bind_target`] (non-struct / generic target).
pub fn emit_config_properties(
    input: &DeriveInput,
    args: &ConfigPropertiesArgs,
) -> Result<TokenStream, EmitError> {
    let ident = &input.ident;
    let bind = emit_bind_target(input)?;
    let fields = struct_fields(input)?;

    let mangled = mangle(&ident.to_string());
    let row_ident = format_ident!("__LEAF_CONFIG_META_{}", mangled);
    let group_ident = format_ident!("__LEAF_CONFIG_GROUP_{}", mangled);
    let props_ident = format_ident!("__LEAF_CONFIG_PROPS_{}", mangled);
    // The C2 Tier-2 bind thunk is PUBLIC under the deterministic
    // `__leaf_config_bind_<Ident>` pairing name (keyed on the raw ident) so
    // leaf-boot's App<Wired>::validate can pair the config bean's Descriptor with its
    // pure-projection bind+JSR recipe (the same shape as the `__leaf_seed_<Ident>`
    // ProviderSeed pairing the assembly pass joins).
    let bind_thunk_ident = format_ident!("__leaf_config_bind_{}", mangled);
    let bind_row_ident = format_ident!("__LEAF_CONFIG_BIND_PAIRING_{}", mangled);

    let prefix = &args.prefix;
    let ident_str = ident.to_string();
    let bind_thunk = emit_config_bind_thunk(ident, prefix, &bind_thunk_ident, args.validate);

    // The config bean's AUTO_CONFIGS registration (the Descriptor + seed + the
    // SEED_PAIRINGS row), so the bean auto-registers from the slices alone — no
    // hand-written `AutoConfigCandidate`/descriptor/seed/provider in the binary.
    let bean_registration = emit_config_bean_registration(ident, &ident_str, &mangled);

    // The const ContractId, module-qualified at the use site (same as components).
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident_str)
        )
    };

    let property_rows = fields.iter().map(|f| {
        let canonical = &f.canonical;
        let ty_str = render_type(&f.ty);
        quote! {
            ::leaf_core::Property {
                name: #canonical,
                ty: #ty_str,
                description: ::core::option::Option::None,
                default: ::core::option::Option::None,
                deprecation: ::core::option::Option::None,
                hints: &[],
                origin: ::leaf_core::CodeSpan { file: ::core::file!(), line: ::core::line!(), column: ::core::column!() },
            }
        }
    });

    Ok(quote! {
        #bind

        // ── the C2 Tier-2 pure-projection bind+JSR thunk (the validate-time path) ──
        #bind_thunk

        // ── the rich const ConfigGroup documenting the bound keys (leaf metadata) ──
        #[allow(non_upper_case_globals)]
        static #props_ident: &[::leaf_core::Property] = &[ #(#property_rows),* ];
        #[allow(non_upper_case_globals)]
        pub const #group_ident: ::leaf_core::ConfigGroup = ::leaf_core::ConfigGroup {
            prefix: #prefix,
            type_name: ::core::concat!(::core::module_path!(), "::", #ident_str),
            description: ::core::option::Option::None,
            properties: #props_ident,
            contract: #contract,
        };

        // ── the minimal ConfigMetadataRow anti-DCE anchor on CONFIG_METADATA ──
        // CROSS-CRATE re-export: the attr is named through leaf-core's
        // `pub use linkme;` as `::leaf_core::linkme::distributed_slice` and
        // `#[linkme(crate = ::leaf_core::linkme)]` redirects linkme's runtime path,
        // so a contributing crate needs NO direct `linkme` dep (same pattern as
        // COMPONENTS).
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::CONFIG_METADATA)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::ConfigMetadataRow = ::leaf_core::ConfigMetadataRow {
            contract: #contract,
            prefix: #prefix,
        };

        // ── the bind-thunk pairing on CONFIG_BIND_PAIRINGS (auto-collect substrate) ──
        // Submit the `__leaf_config_bind_<Ident>` thunk keyed by ContractId so the C2
        // validate sub-pass finds it with no hand-assembled `.with_config_properties`.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::CONFIG_BIND_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #bind_row_ident: ::leaf_core::ConfigBindPairingRow =
            ::leaf_core::ConfigBindPairingRow {
                contract: #contract,
                thunk: #bind_thunk_ident,
            };

        // ── the AUTO_CONFIGS Descriptor + seed so the config bean auto-registers ──
        #bean_registration
    })
}

/// Emit the config bean's AUTO_CONFIGS REGISTRATION — the const `Descriptor` (at
/// `CandidateRole::FALLBACK`, so a user `@Component` of the same type supersedes), the
/// default-returning `Provider` + `ProviderSeed`, and the `SEED_PAIRINGS` row — so a
/// `#[config_properties]` bean is a registered, resolvable bean from the slices ALONE
/// (no hand-written `AutoConfigCandidate`/descriptor/seed in the binary).
///
/// The provider yields `<Ident as Default>::default()`: the REAL bound value is
/// PRE-BOUND into the slot by the C2 `__leaf_config_bind_<Ident>` thunk at validate
/// (before refresh R5), so this `provide` is the never-invoked fallback that only
/// makes the row constructible (the seed JOIN is total).
fn emit_config_bean_registration(
    ident: &syn::Ident,
    ident_str: &str,
    mangled: &syn::Ident,
) -> TokenStream {
    let meta_ident = format_ident!("__LEAF_CONFIG_BEAN_META_{}", mangled);
    let provider_ident = format_ident!("__LeafConfigProvider_{}", mangled);
    let desc_ident = format_ident!("__LEAF_CONFIG_DESCRIPTOR_{}", mangled);
    let seed_ident = format_ident!("__leaf_seed_{}", mangled);
    let seed_row_ident = format_ident!("__LEAF_CONFIG_SEED_PAIRING_{}", mangled);
    let declared_name = leaf_core::derive_default_name(ident_str).into_owned();
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident_str)
        )
    };
    quote! {
        // The config bean's flat AnnotationMetadata at CandidateRole::FALLBACK (the
        // auto-config soft override — a user bean of the same type wins).
        #[allow(non_upper_case_globals)]
        static #meta_ident: ::leaf_core::AnnotationMetadata = ::leaf_core::AnnotationMetadata {
            qualifiers: &[],
            markers: &[],
            depends_on: &[],
            candidate_role: ::leaf_core::CandidateRole::FALLBACK,
            autowire_candidate: true,
        };

        // The default-returning Provider (the C2 thunk pre-binds the real value before
        // R5, so this is the never-invoked fallback making the row constructible).
        #[allow(non_camel_case_types)]
        struct #provider_ident(::leaf_core::Descriptor);
        impl ::leaf_core::Provider for #provider_ident {
            fn descriptor(&self) -> &::leaf_core::Descriptor {
                &self.0
            }
            fn provide<'a>(
                &'a self,
                _cx: &'a ::leaf_core::ResolveCtx<'a>,
            ) -> ::leaf_core::BoxFuture<'a, ::core::result::Result<::leaf_core::Published, ::leaf_core::LeafError>>
            {
                ::std::boxed::Box::pin(async move {
                    ::core::result::Result::Ok(::leaf_core::Published::shared_value(
                        <#ident as ::core::default::Default>::default(),
                    ))
                })
            }
        }

        #[allow(non_upper_case_globals)]
        pub const #seed_ident: ::leaf_core::ProviderSeed = || {
            ::std::sync::Arc::new(#provider_ident(::leaf_core::Descriptor {
                contract: #contract,
                self_type: const { ::core::any::TypeId::of::<#ident>() },
                provides: &[],
                declared_name: ::core::option::Option::Some(#declared_name),
                aliases: &[],
                scope: ::leaf_core::ScopeDef::SINGLETON,
                role: ::leaf_core::Role::Application,
                meta: &#meta_ident,
                parent: ::core::option::Option::None,
                origin: ::leaf_core::Origin::Native {
                    crate_name: ::core::option::Option::Some(::core::env!("CARGO_PKG_NAME")),
                },
            }))
        };

        // The const Descriptor into the SEPARATE AUTO_CONFIGS slice (the config-properties
        // default lane — never COMPONENTS, so component-scanning never picks it up).
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #desc_ident: ::leaf_core::Descriptor = ::leaf_core::Descriptor {
            contract: #contract,
            self_type: const { ::core::any::TypeId::of::<#ident>() },
            provides: &[],
            declared_name: ::core::option::Option::Some(#declared_name),
            aliases: &[],
            scope: ::leaf_core::ScopeDef::SINGLETON,
            role: ::leaf_core::Role::Application,
            meta: &#meta_ident,
            parent: ::core::option::Option::None,
            origin: ::leaf_core::Origin::Native {
                crate_name: ::core::option::Option::Some(::core::env!("CARGO_PKG_NAME")),
            },
        };

        // The seed pairing so from_slices JOINs the AUTO_CONFIGS row to its seed (the
        // anti-DCE per-bean JOIN — an unconstructible bean must never silently vanish).
        // A config-properties bean is bound from the env (no `#[inject]` ctor), so this
        // is a FIELD-DEFAULT-class seed row.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #seed_row_ident: ::leaf_core::SeedPairingRow =
            ::leaf_core::SeedPairingRow::field_default(#contract, #seed_ident);
    }
}

/// Emit the PUBLIC C2 bind+JSR thunk for a `#[config_properties]` bean: a const
/// `::leaf_core::ConfigBindThunk` (`fn(&Env, StartupValidation) -> ConfigBindOutcome`)
/// named `__leaf_config_bind_<Ident>` that BINDS the bean from the sealed `Env`
/// through its derived `BindTarget` under the canonical `prefix`, returning the bound
/// `::leaf_core::Published` to PRE-BIND into the slot (the C2 Tier-2 path
/// leaf-boot's `App<Wired>::validate` threads) or the aggregated bind faults.
///
/// The thunk is a PURE-PROJECTION bind: it reads ONLY the `&Env` (no `ResolveCtx`),
/// so it is safe to run at validate-time before wiring is live — the SAME binder
/// seam the runtime config-properties provider runs, so dry-run == real bind. The
/// bean is opted into the engine-resolvability seam (`impl ::leaf_core::Bean`) so the
/// bound value publishes as a `Published::shared_value`.
///
/// The JSR `ValidationBindHandler` lives in leaf-validation (out of this codegen
/// unit's leaf-core-only dependency surface); the thunk runs the stock
/// `NoopBindHandler`, so bind/convert faults surface here. When `validate` is set the
/// thunk ALSO runs `::leaf_validation::validate_config(prefix, &__bound)` over the
/// bound value (the bean must `#[derive(Validate)]`) before wrapping in `Published`,
/// short-circuiting with the aggregated `ValidationError` as a bind fault — Spring
/// validates the bound bean INCLUDING defaults, so BOTH the Bound and the
/// Unbound/default arm validate. When `validate` is NOT set the thunk is exactly the
/// prior NoopBindHandler bind (existing non-`Validate` config beans are unaffected).
fn emit_config_bind_thunk(
    ident: &syn::Ident,
    prefix: &str,
    thunk_ident: &syn::Ident,
    validate: bool,
) -> TokenStream {
    // The shared per-arm tail: validate the bound value (opt-in) then publish it. The
    // bound value lives in `__bound` in BOTH arms so one tail serves the Bound arm and
    // the Unbound/default arm — Spring validates the bound bean including defaults.
    let publish = if validate {
        quote! {
            if let ::core::option::Option::Some(__verr) =
                ::leaf_validation::validate_config(#prefix, &__bound)
            {
                return ::core::result::Result::Err(::std::vec![__verr]);
            }
            ::core::result::Result::Ok(::leaf_core::Published::shared_value(__bound))
        }
    } else {
        quote! {
            ::core::result::Result::Ok(::leaf_core::Published::shared_value(__bound))
        }
    };
    quote! {
        // The bean is engine-resolvable so the bound value is a Published::shared_value
        // (the same Bean opt-in a #[component] emits — a #[config_properties] type is
        // registered as a bean too, via the auto-config / config-properties lane).
        impl ::leaf_core::Bean for #ident {}

        #[allow(non_upper_case_globals)]
        pub const #thunk_ident: ::leaf_core::ConfigBindThunk =
            |__env: &::leaf_core::Env, __lever: ::leaf_core::StartupValidation|
                -> ::leaf_core::ConfigBindOutcome
        {
            let _ = __lever; // the bind itself is HARD under every lever (C2 structural)
            let __cps = ::leaf_core::StackCps::new(__env.clone());
            let __conv = ::leaf_core::ConversionService::new();
            let __handler = ::leaf_core::NoopBindHandler;
            let __binder = ::leaf_core::Binder::new(&__cps, &__conv, &__handler);
            let __prefix = match ::leaf_core::CanonicalName::parse(#prefix) {
                ::core::result::Result::Ok(__p) => __p,
                ::core::result::Result::Err(__e) => {
                    return ::core::result::Result::Err(::std::vec![
                        ::leaf_core::LeafError::new(::leaf_core::ErrorKind::BindError).caused_by(
                            ::leaf_core::Cause::plain(
                                "binding @ConfigurationProperties",
                                ::std::format!("invalid prefix `{}`: {}", #prefix, __e),
                            )
                        )
                    ]);
                }
            };
            match __binder.bind::<#ident>(&__prefix) {
                ::leaf_core::BindResult::Bound(__bound) => {
                    #publish
                }
                // Absent config is NOT an error — bind the JavaBean default-filled value
                // (validated too when opted in: Spring validates defaults as well).
                ::leaf_core::BindResult::Unbound => {
                    let __bound = <#ident as ::core::default::Default>::default();
                    #publish
                }
                ::leaf_core::BindResult::Failed(__e) => {
                    ::core::result::Result::Err(::std::vec![__e])
                }
            }
        };
    }
}

// ───────────────────────────── #[value("...")] ──────────────────────────────

/// Lower a `#[value("${k:def}")]` template literal to the const
/// `&[::leaf_core::ValueSegment]` the placeholder engine interprets (delegating to
/// the already-built [`crate::parsers`] `${}`/`#{}` splitter).
///
/// # Errors
/// [`EmitError`] when the body is not a single string literal, or the template is
/// malformed (an unbalanced `${`/`#{`).
pub fn emit_value(attr: TokenStream) -> Result<TokenStream, EmitError> {
    let lit: syn::LitStr = syn::parse2(attr).map_err(|e| EmitError {
        message: format!("#[value] expects a single string-literal template: {e}"),
    })?;
    crate::parsers::parse_and_emit(&lit.value()).map_err(|e| EmitError {
        message: format!("#[value] template: {}", e.message),
    })
}

// ─────────────────────────────── #[converter] ───────────────────────────────

/// Emit the `#[converter]` registration artifact: one `::leaf_core::CatalogRow`
/// anti-DCE anchor into the `CATALOGS` slice keyed on the converter's stable
/// identity. The user supplies the `impl ::leaf_core::Converter`; this wires its
/// discovery.
///
/// `ident` is the converter type's ident; the contract is module-qualified at the
/// definition site (same as components).
#[must_use]
pub fn emit_converter(ident: &str) -> TokenStream {
    let mangled = mangle(ident);
    let row_ident = format_ident!("__LEAF_CATALOG_{}", mangled);
    quote! {
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::CATALOGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::CatalogRow = ::leaf_core::CatalogRow {
            contract: ::leaf_core::ContractId::of(
                ::core::concat!(::core::module_path!(), "::", #ident)
            ),
        };
    }
}

// ─────────────────────────────── helpers ────────────────────────────────────

/// The string body of a `key = "literal"` value, if it is a string literal.
fn str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

/// Map a snake_case field ident to its canonical kebab name (the relaxed-binding
/// key the cursor reads). `max_connections` → `max-connections`.
fn canonical_name(ident: &str) -> String {
    ident.replace('_', "-")
}

/// Render a type to a string (for the `Property.ty` documentation field).
fn render_type(ty: &Type) -> String {
    quote! { #ty }.to_string().split_whitespace().collect::<String>()
}

/// A spans-free, identifier-safe mangling of an ident for emitted helper names.
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, Span::call_site())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn derive(src: &str) -> DeriveInput {
        syn::parse_str(src).expect("a valid derive input")
    }

    // ── derive(BindTarget): the const NodeSchema + bind fold ───────────────────

    #[test]
    fn bind_target_emits_a_const_object_node_schema() {
        let ts = emit_bind_target(&derive("struct ServerProps { port: u16, host: String }"))
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The schema is an Object{JavaBean} with one Field per struct field.
        assert!(s.contains("::leaf_core::NodeSchema::Object{"), "got: {s}");
        assert!(s.contains("method:::leaf_core::BindMethod::JavaBean"), "got: {s}");
        assert!(s.contains(r#"canonical:"port""#), "got: {s}");
        assert!(s.contains(r#"canonical:"host""#), "got: {s}");
    }

    #[test]
    fn bind_target_emits_the_bindtarget_impl_with_a_scalar_fold() {
        let s = flat(&emit_bind_target(&derive("struct P { port: u16 }")).expect("emits"));
        assert!(s.contains("impl::leaf_core::BindTargetforP"), "got: {s}");
        // A scalar field folds through cursor.scalar::<u16>("port").
        assert!(s.contains(r#"__cursor.scalar::<u16>("port")"#), "got: {s}");
        // The const SCHEMA pointer references the emitted schema static.
        assert!(s.contains("constSCHEMA:&'static::leaf_core::NodeSchema"), "got: {s}");
    }

    #[test]
    fn snake_case_field_canonicalizes_to_kebab() {
        let s = flat(&emit_bind_target(&derive("struct P { max_connections: u32 }")).expect("emits"));
        assert!(s.contains(r#"canonical:"max-connections""#), "got: {s}");
        // The cursor reads the kebab canonical key.
        assert!(s.contains(r#"__cursor.scalar::<u32>("max-connections")"#), "got: {s}");
    }

    #[test]
    fn a_vec_field_binds_as_a_list() {
        let s = flat(&emit_bind_target(&derive("struct P { hosts: Vec<String> }")).expect("emits"));
        // The list field folds through cursor.list::<String> and a List node schema.
        assert!(s.contains(r#"__cursor.list::<String>("hosts")"#), "got: {s}");
        assert!(s.contains("::leaf_core::NodeSchema::List(&::leaf_core::NodeSchema::Scalar)"), "got: {s}");
    }

    #[test]
    fn a_nested_struct_field_binds_through_nested() {
        let s = flat(&emit_bind_target(&derive("struct P { server: ServerProps }")).expect("emits"));
        assert!(s.contains(r#"__cursor.nested::<ServerProps>("server")"#), "got: {s}");
        // Its schema references the inner type's derived SCHEMA pointer.
        assert!(s.contains("<ServerPropsas::leaf_core::BindTarget>::SCHEMA"), "got: {s}");
    }

    #[test]
    fn bind_target_rejects_a_non_struct() {
        let err = emit_bind_target(&derive("enum E { A, B }")).expect_err("an enum is rejected");
        assert!(err.message.contains("not a struct"), "got: {}", err.message);
    }

    #[test]
    fn bind_target_rejects_a_generic_target() {
        let err = emit_bind_target(&derive("struct P<T> { inner: T }"))
            .expect_err("a generic target is rejected");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    // ── #[config_properties(prefix=...)] ───────────────────────────────────────

    #[test]
    fn config_properties_parses_the_prefix_arg() {
        let attr: TokenStream = syn::parse_str(r#"prefix = "app.server""#).expect("tokens");
        let args = parse_config_args(attr).expect("parses");
        assert_eq!(args.prefix, "app.server");
    }

    #[test]
    fn config_properties_requires_a_prefix() {
        let err = parse_config_args(TokenStream::new()).expect_err("prefix is required");
        assert!(err.message.contains("prefix"), "got: {}", err.message);
    }

    #[test]
    fn config_properties_rejects_unknown_arg() {
        let attr: TokenStream = syn::parse_str(r#"bogus = "x""#).expect("tokens");
        let err = parse_config_args(attr).expect_err("unknown arg errors");
        assert!(err.message.contains("unknown"), "got: {}", err.message);
    }

    #[test]
    fn config_properties_validate_flag_defaults_off() {
        let attr: TokenStream = syn::parse_str(r#"prefix = "app""#).expect("tokens");
        let args = parse_config_args(attr).expect("parses");
        assert!(!args.validate, "an unflagged config bean does not validate");
    }

    #[test]
    fn config_properties_parses_the_bare_validate_flag() {
        let attr: TokenStream = syn::parse_str(r#"prefix = "app", validate"#).expect("tokens");
        let args = parse_config_args(attr).expect("parses");
        assert_eq!(args.prefix, "app");
        assert!(args.validate, "the bare `validate` flag opts in");
    }

    #[test]
    fn config_properties_rejects_unknown_bare_flag() {
        let attr: TokenStream = syn::parse_str(r#"prefix = "app", bogus"#).expect("tokens");
        let err = parse_config_args(attr).expect_err("an unknown bare flag errors");
        assert!(err.message.contains("bogus"), "got: {}", err.message);
    }

    #[test]
    fn config_properties_unflagged_thunk_has_no_validation() {
        // Gate: an UNFLAGGED config bean's bind thunk wraps the bound value in
        // Published with NO `validate_config` call (existing beans unaffected).
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: false };
        let s = flat(
            &emit_config_properties(&derive("struct AppProps { name: String }"), &args)
                .expect("emits"),
        );
        assert!(!s.contains("validate_config"), "no validation is wired when unflagged: {s}");
    }

    #[test]
    fn config_properties_validate_flag_wires_validate_config_in_both_arms() {
        // The opt-in: a `validate`-flagged bean's bind thunk runs
        // `::leaf_validation::validate_config(prefix, &bound)` BEFORE wrapping in
        // Published, in BOTH the Bound and the Unbound/default arm (Spring validates
        // the bound bean INCLUDING defaults), surfacing the fault as Err(vec![__verr]).
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: true };
        let ts = emit_config_properties(&derive("struct AppProps { name: String }"), &args)
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The validate call is emitted with the absolute leaf_validation path + prefix.
        assert!(
            s.contains(r#"::leaf_validation::validate_config("app",&__bound)"#),
            "got: {s}"
        );
        // It short-circuits with the aggregated fault as a single-element Err vec.
        assert!(s.contains("return::core::result::Result::Err(::std::vec![__verr]);"), "got: {s}");
        // BOTH arms validate: the Bound arm names `__bound`, the Unbound/default arm
        // binds the default into `__bound` first, so `validate_config(.., &__bound)`
        // appears TWICE (once per arm).
        assert_eq!(
            s.matches(r#"::leaf_validation::validate_config("app",&__bound)"#).count(),
            2,
            "the bound arm AND the default arm both validate: {s}"
        );
    }

    #[test]
    fn config_properties_emits_a_config_metadata_row() {
        // The headline: a #[config_properties] type emits a CONFIG_METADATA row
        // carrying the prefix + the module-qualified contract id.
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: false };
        let ts = emit_config_properties(&derive("struct AppProps { name: String }"), &args)
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::CONFIG_METADATA)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::ConfigMetadataRow"), "got: {s}");
        assert!(s.contains(r#"prefix:"app""#), "got: {s}");
        // The contract is module-qualified at the definition site.
        assert!(
            s.contains(r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","AppProps"))"#),
            "got: {s}"
        );
    }

    #[test]
    fn config_properties_auto_registers_the_bean_into_auto_configs_with_a_seed() {
        // The auto-collect closure: a #[config_properties] type ALSO emits its OWN
        // AUTO_CONFIGS Descriptor (at CandidateRole::FALLBACK) + a default-returning
        // ProviderSeed + the SEED_PAIRINGS row — so the bean registers + binds purely
        // from the slices, with no hand-written AutoConfigCandidate/descriptor/seed.
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: false };
        let ts = emit_config_properties(&derive("struct AppProps { name: String }"), &args)
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The Descriptor rides the SEPARATE AUTO_CONFIGS slice (never COMPONENTS).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]"),
            "got: {s}"
        );
        // …at CandidateRole::FALLBACK (a user @Component of the same type supersedes).
        assert!(s.contains("candidate_role:::leaf_core::CandidateRole::FALLBACK"), "got: {s}");
        // The PUBLIC __leaf_seed_<Ident> ProviderSeed (the assembly JOIN key).
        assert!(s.contains("pubconst__leaf_seed_AppProps:::leaf_core::ProviderSeed"), "got: {s}");
        // …yielding the JavaBean default (the C2 thunk pre-binds the real value at validate).
        assert!(s.contains("<AppPropsas::core::default::Default>::default()"), "got: {s}");
        // …JOINed by the SEED_PAIRINGS row (so from_slices wires it, no `.with_seeds`).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]"),
            "got: {s}"
        );
        // A config-properties bean is env-bound (no `#[inject]` ctor), so its seed row
        // is a FIELD-DEFAULT-class row.
        assert!(
            s.contains("::leaf_core::SeedPairingRow::field_default("),
            "got: {s}"
        );
        assert!(s.contains("__leaf_seed_AppProps"), "got: {s}");
    }

    #[test]
    fn config_properties_also_emits_the_bind_target_and_a_config_group() {
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: false };
        let s = flat(
            &emit_config_properties(&derive("struct AppProps { name: String, port: u16 }"), &args)
                .expect("emits"),
        );
        // The same expansion derives BindTarget…
        assert!(s.contains("impl::leaf_core::BindTargetforAppProps"), "got: {s}");
        // …and a rich ConfigGroup documenting each bound key.
        assert!(s.contains("::leaf_core::ConfigGroup"), "got: {s}");
        assert!(s.contains("::leaf_core::Property"), "got: {s}");
        assert!(s.contains(r#"name:"name""#), "got: {s}");
        assert!(s.contains(r#"name:"port""#), "got: {s}");
        // The property type is rendered as a string for the metadata.
        assert!(s.contains(r#"ty:"u16""#), "got: {s}");
    }

    #[test]
    fn config_properties_emits_a_public_bind_thunk_pairing_const() {
        // The C2 Tier-2 path: a #[config_properties] type emits a PUBLIC const bind
        // thunk (`__leaf_config_bind_<Ident>`) of the macro-emitted
        // `::leaf_core::ConfigBindThunk` type, so leaf-boot's App<Wired>::validate can
        // JOIN it by ContractId and thread the REAL macro-emitted thunk (the same
        // pairing-const pattern as the __leaf_seed_<Ident> ProviderSeed).
        let args = ConfigPropertiesArgs { prefix: "app".into(), validate: false };
        let ts = emit_config_properties(&derive("struct AppProps { title: String, workers: u16 }"), &args)
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The thunk is a PUBLIC const under the deterministic pairing name.
        assert!(
            s.contains("pubconst__leaf_config_bind_AppProps:::leaf_core::ConfigBindThunk"),
            "got: {s}"
        );
        // It binds the bean from the env under the prefix through the derived
        // BindTarget (the pure-projection bind, &Env only), via the one Binder seam.
        assert!(s.contains("::leaf_core::Binder::new"), "got: {s}");
        assert!(s.contains("bind::<AppProps>"), "got: {s}");
        // The prefix is the parsed canonical key prefix.
        assert!(s.contains(r#"CanonicalName::parse("app")"#), "got: {s}");
        // It produces a Published on success (pre-bound into the slot) — the bean is
        // opted into the engine-resolvability seam so Published::shared_value applies.
        assert!(s.contains("::leaf_core::Published::shared_value"), "got: {s}");
        assert!(s.contains("impl::leaf_core::BeanforAppProps{}"), "got: {s}");
        // The thunk is ALSO auto-collected into CONFIG_BIND_PAIRINGS keyed by
        // ContractId (the COMPONENTS auto-collect substrate, extended) so the C2
        // validate sub-pass finds it with no hand-assembled `.with_config_properties`.
        assert!(
            s.contains(
                "#[::leaf_core::linkme::distributed_slice(::leaf_core::CONFIG_BIND_PAIRINGS)]"
            ),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::ConfigBindPairingRow{contract:"), "got: {s}");
        assert!(s.contains("thunk:__leaf_config_bind_AppProps"), "got: {s}");
    }

    // ── #[value("...")] ────────────────────────────────────────────────────────

    #[test]
    fn value_template_lowers_to_const_value_segments() {
        let attr: TokenStream = syn::parse_str(r#""${app.port:8080}""#).expect("tokens");
        let ts = emit_value(attr).expect("emits");
        let s = flat(&ts);
        // Delegates to the parsers splitter → a const &[::leaf_core::Segment]/ValueSegment.
        assert!(s.contains("::leaf_core::"), "got: {s}");
        // The placeholder key + default survive the split.
        assert!(s.contains("app.port") || s.contains(r#""app.port""#), "got: {s}");
        syn::parse2::<syn::Expr>(ts).expect("a valid const expr");
    }

    #[test]
    fn value_rejects_a_non_string_body() {
        let attr: TokenStream = syn::parse_str("42").expect("tokens");
        let err = emit_value(attr).expect_err("a non-string body errors");
        assert!(err.message.contains("string-literal"), "got: {}", err.message);
    }

    #[test]
    fn value_rejects_a_malformed_template() {
        let attr: TokenStream = syn::parse_str(r#""${unbalanced""#).expect("tokens");
        let err = emit_value(attr).expect_err("an unbalanced template errors");
        assert!(err.message.contains("template"), "got: {}", err.message);
    }

    // ── #[converter] ───────────────────────────────────────────────────────────

    #[test]
    fn converter_emits_a_catalog_row() {
        let ts = emit_converter("DurationConverter");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::CATALOGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::CatalogRow"), "got: {s}");
        assert!(
            s.contains(r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","DurationConverter"))"#),
            "got: {s}"
        );
    }
}
