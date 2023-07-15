use proc_macro_error::{abort_call_site, abort};
use quote::ToTokens;
use syn::{meta::ParseNestedMeta, Path, Item, Attribute, Meta, parse_quote};
use crate::shared::unreachable;

/// Since we want to be maximally interoperable, we need to handle attributes in a special way.
/// We use `#[bitsize]` as a sort of scope for all attributes below it and
/// the whole family of `-Bits` macros only works when used in that scope.
/// 
/// Let's visualize why this is the case, starting with some user-code:
/// ```ignore
/// #[bitsize(6)]
/// #[derive(Clone, Copy, PartialEq, DebugBits, FromBits)]
/// struct Example {
///     field1: u2,
///     field2: u4,
/// }
/// ```
/// First, the attributes get sorted, depending on their name.
/// Every attribute in need of field information gets resolved first,
/// in this case `DebugBits` and `FromBits`.
/// 
/// Now, after resolving all `before_compression` attributes, the halfway-resolved
/// code looks like this:
/// ```ignore
/// #[bilge::bitsize_internal(6)]
/// #[derive(Clone, Copy, PartialEq)]
/// struct Example {
///     field1: u2,
///     field2: u4,
/// }
/// ```
/// This `#[bitsize_internal]` attribute is the one actually doing the compression and generating
/// all the getters, setters and a constructor.
/// 
/// Finally, the struct ends up like this (excluding the generated impl blocks):
/// ```ignore
/// struct Example {
///     value: u6,
/// }
/// ```
pub struct SplitAttributes {
    pub before_compression: Vec<Attribute>,
    pub after_compression: Vec<Attribute>,
}

/// Split item attributes into those applied before bitfield-compression and those applied after.
/// Also, abort on any invalid configuration.
/// 
/// Any derives with suffix `Bits` will be able to access field information.
/// This way, users of `bilge` can define their own derives working on the uncompressed bitfield.
pub fn split_item_attributes(item: &Item) -> SplitAttributes {
    let attrs = match item {
        Item::Enum(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        _ => abort_call_site!("item is not a struct or enum"; help = "`#[bitsize]` can only be used on structs and enums"),
    };

    let parsed = attrs.iter().map(parse_attribute);
    
    let is_struct = matches!(item, Item::Struct(..));
    
    let mut from_bytes = None;
    let mut has_frombits = false;

    let mut before_compression = vec![];
    let mut after_compression = vec![];

    for parsed_attr in parsed {
        match parsed_attr {
            ParsedAttribute::DeriveList(derives) => {
                for derive in derives {
                    // NOTE: we could also handle `::{path}`
                    match derive.to_string().as_str() {
                        "FromBytes" | "zerocopy :: FromBytes" => from_bytes = Some(derive.clone()),
                        "FromBits" | "bilge :: FromBits" => has_frombits = true,
                        "Debug" | "fmt :: Debug" | "core :: fmt :: Debug" | "std :: fmt :: Debug" if is_struct => {
                            abort!(derive.0, "use derive(DebugBits) for structs")
                        }
                        _ => {}
                    };

                
                    if derive.is_bitfield_derive() {
                        // this handles the custom derives
                        before_compression.push(derive.into_attribute());
                    } else {
                        // It is most probable that basic derive macros work if we put them on after compression
                        after_compression.push(derive.into_attribute());
                    }
                }
            },

            ParsedAttribute::BitsizeInternal(attr) => {
                abort!(attr, "remove bitsize_internal"; help = "attribute bitsize_internal can only be applied internally by the bitsize macros")
            },

            ParsedAttribute::Other(attr) => {
                // I don't know with which attrs I can hit Path and NameValue,
                // so let's just put them on after compression.
                after_compression.push(attr.clone())
            },
        };
    }

    if let Some(from_bytes) = from_bytes {
        // TODO: is this error also applicable to enums?
        if !has_frombits && is_struct {
            abort!(from_bytes.0, "a bitfield struct with zerocopy::FromBytes also needs to have FromBits")
        }
    }

    // currently, enums don't need special handling - so just put all attributes before compression
    //
    // TODO: this doesn't preserve order, in the sense that an "after compression" attribute will appear
    // after all "before compression" attributes. is that okay?
    if !is_struct {
        before_compression.append(&mut after_compression)
    }
    
    SplitAttributes { before_compression, after_compression }    
}

fn parse_attribute(attribute: &Attribute) -> ParsedAttribute {
    match &attribute.meta {
        Meta::List(list) if list.path.is_ident("derive") => {
            let mut derives = Vec::new();
            let add_derive = |meta: ParseNestedMeta| {
                let derive = Derive(meta.path);
                derives.push(derive);

                Ok(())
            };

            list.parse_nested_meta(add_derive).unwrap_or_else(|e| abort!(list.tokens, "failed to parse derive: {}", e));

            ParsedAttribute::DeriveList(derives)
        }

        meta if contains_anywhere(meta, "bitsize_internal") => ParsedAttribute::BitsizeInternal(attribute),

        _ => ParsedAttribute::Other(attribute),
    }
}

/// a crude approximation of things we currently consider in item attributes
enum ParsedAttribute<'attr> {
    DeriveList(Vec<Derive>),
    BitsizeInternal(&'attr Attribute),
    Other(&'attr Attribute),
}

/// the path of a single derive attribute, parsed from a list which may have contained several
#[derive(Clone)]
struct Derive(Path);

impl ToString for Derive {
    fn to_string(&self) -> String {
        self.0.to_token_stream().to_string()
    }
}

impl Derive {
    /// a new `#[derive]` attribute containing only this derive
    fn into_attribute(self) -> Attribute {
        let path = self.0;
        parse_quote! { #[derive(#path)] }
    }

    /// by `bilge` convention, any derive satisfying this condition is able
    /// to access bitfield structure information pre-compression, 
    /// allowing for user derives
    /// 
    /// TODO: this method name is bikeshedable
    fn is_bitfield_derive(&self) -> bool {
        let last_segment = self.0.segments.last().unwrap_or_else(|| unreachable(()));

        last_segment.ident.to_string().ends_with("Bits")
    }
}

/// slightly hacky. attempts to recognize cases where an ident is deeply-nested in the meta.
fn contains_anywhere(meta: &Meta, ident: &str) -> bool {
    meta.to_token_stream().to_string().contains(ident)
}