#![forbid(unsafe_code)]

use quote::quote;
use syn::{Attribute, Data, DeriveInput, Path, Type, parse_macro_input};
use proc_macro::TokenStream;

#[proc_macro_derive(RegisterMemoryRead)]
pub fn derive_memory_read(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, .. } = parse_macro_input!(input as DeriveInput);

    TokenStream::from(quote! {
        impl RegisterMemoryRead for #ident {}
    })
}

#[proc_macro_derive(RegisterMemoryWrite)]
pub fn derive_memory_write(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, .. } = parse_macro_input!(input as DeriveInput);

    TokenStream::from(quote! {
        impl RegisterMemoryWrite for #ident {}
    })
}

enum FieldAttribute {
    Enum {
        field_ty: Path,
        offset: u8,
        length: u8,
    },
    Int {
        field_ty: Path,
        offset: u8,
        length: u8,
    },
    Bool {
        field_ty: Path,
        offset: u8,
    },
}

impl FieldAttribute {
    fn from_attribute(attr: Attribute) -> syn::Result<Option<Self>> {
        struct RangeAttribute {
            field_ty: Path,
            offset: u8,
            end: u8,
        }

        impl syn::parse::Parse for RangeAttribute {
            fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
                Ok(RangeAttribute {
                    field_ty: input.parse()?,
                    offset: {
                        let _: syn::Token![=] = input.parse()?;
                        let offset: syn::LitInt = input.parse()?;
                        offset.to_string().parse().unwrap()
                    },
                    end: {
                        let _: syn::Token![.] = input.parse()?;
                        let _: syn::Token![.] = input.parse()?;
                        let end: syn::LitInt = input.parse()?;
                        end.to_string().parse().unwrap()
                    },
                })
            }
        }

        struct OffsetAttribute {
            field_ty: Path,
            offset: u8,
        }

        impl syn::parse::Parse for OffsetAttribute {
            fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
                Ok(OffsetAttribute {
                    field_ty: input.parse()?,
                    offset: {
                        let _: syn::Token![=] = input.parse()?;
                        let offset: syn::LitInt = input.parse()?;
                        offset.to_string().parse().unwrap()
                    },
                })
            }
        }

        if attr.path.segments.len() == 1 {
            // field_enum or field_int or field_bool
            let discriminant = attr.path.segments.first().unwrap().ident.to_string();
            match discriminant.as_str() {
                "field_enum" => {
                    let RangeAttribute {
                        field_ty,
                        offset,
                        end,
                    } = attr.parse_args()?;
                    Ok(Some(FieldAttribute::Enum {
                        field_ty,
                        offset,
                        length: end - offset,
                    }))
                },
                "field_int" => {
                    let RangeAttribute {
                        field_ty,
                        offset,
                        end,
                    } = attr.parse_args()?;
                    Ok(Some(FieldAttribute::Int {
                        field_ty,
                        offset,
                        length: end - offset,
                    }))
                },
                "field_bool" => {
                    let OffsetAttribute { field_ty, offset } = attr.parse_args()?;
                    Ok(Some(FieldAttribute::Bool { field_ty, offset }))
                },
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }
}

fn get_ty(data: Data, ident: &syn::Ident) -> syn::Result<Type> {
    match data {
        Data::Struct(d) if d.fields.len() == 1 => {
            let field = d.fields.iter().next().unwrap();
            Ok(field.ty.clone())
        },
        _ => {
            let msg = "expected struct and one field";
            Err(syn::Error::new_spanned(ident, msg))
        },
    }
}

#[proc_macro_derive(RegisterGetField, attributes(field_enum, field_int, field_bool))]
pub fn derive_get_field(input: TokenStream) -> TokenStream {
    let DeriveInput {
        attrs, ident, data, ..
    } = parse_macro_input!(input as DeriveInput);

    let ty = match get_ty(data, &ident) {
        Ok(ty) => ty,
        Err(error) => return error.to_compile_error().into(),
    };

    let mut tokens = quote! {};
    for attr in attrs {
        match FieldAttribute::from_attribute(attr) {
            Err(error) => return error.into_compile_error().into(),
            Ok(None) => continue,
            Ok(Some(FieldAttribute::Enum {
                field_ty,
                offset,
                length,
            })) => {
                let _ = (length, offset, &ty);
                return syn::Error::new_spanned(field_ty, "enum not implemented")
                    .to_compile_error()
                    .into();
            },
            Ok(Some(FieldAttribute::Int {
                field_ty,
                offset,
                length,
            })) => {
                let _ = (length, offset, &ty);
                return syn::Error::new_spanned(field_ty, "int not implemented")
                    .to_compile_error()
                    .into();
            },
            Ok(Some(FieldAttribute::Bool { field_ty, offset })) => {
                tokens = quote! {
                    #tokens
                    impl RegisterGetField<#field_ty> for #ident {
                        fn get_field(&self) -> #field_ty {
                            #field_ty((self.0 & (1 << #offset)) != 0)
                        }
                    }
                }
            },
        }
    }

    TokenStream::from(tokens)
}

#[proc_macro_derive(RegisterSetField, attributes(field_enum, field_int, field_bool))]
pub fn derive_set_field(input: TokenStream) -> TokenStream {
    let DeriveInput {
        attrs, ident, data, ..
    } = parse_macro_input!(input as DeriveInput);

    let ty = match get_ty(data, &ident) {
        Ok(ty) => ty,
        Err(error) => return error.to_compile_error().into(),
    };

    let mut tokens = quote! {};
    for attr in attrs {
        let (field_ty, offset, length, value) = match FieldAttribute::from_attribute(attr) {
            Err(error) => return error.into_compile_error().into(),
            Ok(None) => continue,
            Ok(Some(FieldAttribute::Enum {
                field_ty,
                offset,
                length,
            })) => (field_ty, offset, length, quote! { (field as #ty) }),
            Ok(Some(FieldAttribute::Int {
                field_ty,
                offset,
                length,
            })) => (field_ty, offset, length, quote! { <#ty>::from(field.0) }),
            Ok(Some(FieldAttribute::Bool { field_ty, offset })) => (
                field_ty,
                offset,
                1,
                quote! { (if field.0 { 1 } else { 0 }) },
            ),
        };
        tokens = quote! {
            #tokens
            impl RegisterSetField<#field_ty> for #ident {
                fn set_field(self, field: #field_ty) -> Self {
                    let mask = if #length as usize == core::mem::size_of::<#ty>() * 8 {
                        #ty::MAX
                    } else {
                        (1 << #length) - 1
                    };
                    #ident(self.0 & !(mask << #offset) | (#value << #offset))
                }
            }
        };
    }

    TokenStream::from(tokens)
}
