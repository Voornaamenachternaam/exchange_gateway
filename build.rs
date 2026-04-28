// build.rs
use heck::ToSnakeCase;
use quote::quote;
use syn::Ident;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Generate protocol action name constants using syn/quote
    // This creates type-safe action identifiers from EWS operation names
    let ews_actions = [
        "GetFolder",
        "FindFolder",
        "FindItem",
        "GetItem",
        "CreateItem",
        "UpdateItem",
        "DeleteItem",
        "SyncFolderItems",
        "SyncFolderHierarchy",
        "GetRoomLists",
        "GetRooms",
        "GetDelegate",
        "AddDelegate",
        "RemoveDelegate",
        "UpdateDelegate",
        "CreateAttachment",
        "GetAttachment",
        "DeleteAttachment",
    ];

    let action_match_arms: Vec<_> = ews_actions
        .iter()
        .map(|action| {
            let _ident = Ident::new(action, proc_macro2::Span::call_site());
            let snake = action.to_snake_case();
            let const_ident = Ident::new(
                &format!("ACTION_{}", action.to_snake_case().to_uppercase()),
                proc_macro2::Span::call_site(),
            );
            quote! {
                pub const #const_ident: &str = #snake;
            }
        })
        .collect();

    let generated = quote! {
        /// Auto-generated EWS action name constants.
        /// Produced by build.rs using syn/quote from protocol definitions.
        pub struct EwsActionNames;
        impl EwsActionNames {
            #(#action_match_arms)*
        }
    };

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest_path = std::path::Path::new(&out_dir).join("ews_actions_generated.rs");
    std::fs::write(&dest_path, generated.to_string()).unwrap();
}