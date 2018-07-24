#![recursion_limit = "128"]

#[macro_use]
extern crate failure;
extern crate graphql_parser;
extern crate heck;
extern crate proc_macro;
extern crate proc_macro2;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate syn;
#[macro_use]
extern crate quote;
extern crate graphql_config;

use proc_macro2::TokenStream;

mod constants;
mod enums;
mod field_type;
mod fragments;
mod inputs;
mod interfaces;
mod introspection_response;
mod objects;
mod query;
mod read_graphql_config;
mod scalars;
mod schema;
mod selection;
mod shared;
mod unions;
mod variables;

#[cfg(test)]
mod tests;

use heck::*;
use proc_macro2::{Ident, Span};

#[proc_macro_derive(GraphQLQuery, attributes(graphql))]
pub fn graphql_query_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = TokenStream::from(input);
    let ast = syn::parse2(input).expect("Derive input is well formed");
    let gen = impl_gql_query(&ast).unwrap();
    gen.into()
}

fn read_file(
    path: impl AsRef<::std::path::Path> + ::std::fmt::Debug,
) -> Result<String, failure::Error> {
    use std::io::prelude::*;

    let mut out = String::new();
    let mut file = ::std::fs::File::open(&path).map_err(|io_err| {
        let err: failure::Error = io_err.into();
        err.context(format!(
            r#"
            Could not find file with path: {:?}
            Hint: file paths in the GraphQLQuery attribute are relative to the project root (location of the Cargo.toml). Example: query_path = "src/my_query.graphql".
            "#,
            path
        ))
    })?;
    file.read_to_string(&mut out)?;
    Ok(out)
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct FullResponse<T> {
    data: T,
}

fn find_schema_path(input: &syn::DeriveInput) -> Result<String, failure::Error> {
    extract_attr(input, "schema_path")
        .or_else(|_err| {
            read_graphql_config::read_graphql_config()
                .and_then(|config| {
                    config
                        .root
                        .schema_path
                        .ok_or_else(|| format_err!("Missing schemaPath in .graphqlconfig"))
                })
                .map(|path| path.to_string_lossy().into_owned())
        })
        .map_err(|_| {
            format_err!(r#"
                Could not find a schema for the {} query either from the attribute on the query or in a .graphqlconfig file at the project root.
                "#,
                input.ident
            )
        })
}

fn impl_gql_query(input: &syn::DeriveInput) -> Result<TokenStream, failure::Error> {
    let cargo_manifest_dir =
        ::std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR env variable is defined");

    let module_name = Ident::new(&input.ident.to_string().to_snake_case(), Span::call_site());
    let struct_name = &input.ident;

    let query_path = extract_attr(input, "query_path")?;

    let schema_path = find_schema_path(&input)?;

    // We need to qualify the query with the path to the crate it is part of
    let query_path = format!("{}/{}", cargo_manifest_dir, query_path);
    let query_string = read_file(&query_path)?;
    let query = graphql_parser::parse_query(&query_string)?;

    // We need to qualify the schema with the path to the crate it is part of
    let schema_path = ::std::path::Path::new(&cargo_manifest_dir).join(schema_path);
    let schema_string = read_file(&schema_path)?;

    let extension = schema_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("INVALID");

    let schema = match extension {
        "graphql" | "gql" => {
            let s = graphql_parser::schema::parse_schema(&schema_string)?;
            schema::Schema::from(s)
        }
        "json" => {
            let parsed: FullResponse<introspection_response::IntrospectionResponse> = ::serde_json::from_str(&schema_string)?;
            schema::Schema::from(parsed.data)
        }
        extension => panic!("Unsupported extension for the GraphQL schema: {} (only .json and .graphql are supported)", extension)
    };

    let schema_output = schema.response_for_query(query)?;

    let result = quote!(
        pub mod #module_name {
            #![allow(non_camel_case_types)]
            #![allow(non_snake_case)]
            #![allow(dead_code)]

            use serde;

            pub const QUERY: &'static str = #query_string;

            #schema_output
        }

        impl<'de> ::graphql_client::GraphQLQuery<'de> for #struct_name {
            type Variables = #module_name::Variables;
            type ResponseData = #module_name::ResponseData;

            fn build_query(variables: Self::Variables) -> ::graphql_client::GraphQLQueryBody<Self::Variables> {
                ::graphql_client::GraphQLQueryBody {
                    variables,
                    query: #module_name::QUERY,
                }

            }
        }
    );

    Ok(result)
}

fn extract_attr(ast: &syn::DeriveInput, attr: &str) -> Result<String, failure::Error> {
    let attributes = &ast.attrs;
    let attribute = attributes
        .iter()
        .find(|attr| {
            let path = &attr.path;
            quote!(#path).to_string() == "graphql"
        })
        .ok_or_else(|| format_err!("The graphql attribute is missing"))?;
    if let syn::Meta::List(items) = &attribute
        .interpret_meta()
        .expect("Attribute is well formatted")
    {
        for item in items.nested.iter() {
            if let syn::NestedMeta::Meta(syn::Meta::NameValue(name_value)) = item {
                let syn::MetaNameValue { ident, lit, .. } = name_value;
                if ident == attr {
                    if let syn::Lit::Str(lit) = lit {
                        return Ok(lit.value());
                    }
                }
            }
        }
    }

    Err(format_err!("attribute not found"))?
}
