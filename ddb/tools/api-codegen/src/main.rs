//! Deterministic generator and drift checker for the public DDB API contract.

use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use prost::Message;
use prost_types::{
    field_descriptor_proto::{Label, Type},
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorSet,
};
use serde_json::{json, Map, Value};

const PACKAGE: &str = ".ddb.api.v2";
const PROTO_FILES: &[&str] = &[
    "ddb/api/v2/common.proto",
    "ddb/api/v2/extension.proto",
    "ddb/api/v2/resources.proto",
    "ddb/api/v2/debugger_service.proto",
    "ddb/api/v2/event_service.proto",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Generate,
    Check,
}

fn main() -> Result<()> {
    let mode = parse_mode()?;
    let workspace = workspace_root()?;
    let generated = generate_into_temp(&workspace)?;

    let outputs = [
        (
            generated.path().join("ddb.api.v2.rs"),
            workspace.join("api-types/src/generated/ddb.api.v2.rs"),
        ),
        (
            generated.path().join("ddb.api.v2.serde.rs"),
            workspace.join("api-types/src/generated/ddb.api.v2.serde.rs"),
        ),
        (
            generated.path().join("ddb_api_v2_descriptor.bin"),
            workspace.join("api-types/descriptor/ddb_api_v2_descriptor.bin"),
        ),
        (
            generated.path().join("openapi-v2.json"),
            workspace.join("docs/api/generated/openapi-v2.json"),
        ),
        (
            generated.path().join("asyncapi-v2.json"),
            workspace.join("docs/api/generated/asyncapi-v2.json"),
        ),
        (
            generated.path().join("grpc/ddb.api.v2.rs"),
            workspace.join("api-grpc/src/generated/ddb.api.v2.rs"),
        ),
        (
            generated.path().join("typescript/types.ts"),
            workspace.join("sdk/typescript/src/generated/types.ts"),
        ),
        (
            generated.path().join("typescript/contract.ts"),
            workspace.join("sdk/typescript/src/generated/contract.ts"),
        ),
        (
            generated.path().join("python/types.py"),
            workspace.join("sdk/python/src/ddb_api/generated/types.py"),
        ),
        (
            generated.path().join("python/contract.py"),
            workspace.join("sdk/python/src/ddb_api/generated/contract.py"),
        ),
    ];

    match mode {
        Mode::Generate => {
            for (source, destination) in outputs {
                copy_if_changed(&source, &destination)?;
            }
            println!("DDB API v2 contract artifacts are up to date");
        }
        Mode::Check => {
            let mut stale = Vec::new();
            for (source, destination) in outputs {
                if !files_equal(&source, &destination)? {
                    stale.push(destination);
                }
            }

            if !stale.is_empty() {
                let paths = stale
                    .iter()
                    .map(|path| format!("  - {}", display_relative(&workspace, path)))
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "generated API artifacts are missing or stale:\n{paths}\n\
                     run cargo run -p ddb-api-codegen -- generate"
                );
            }

            println!("DDB API v2 generated artifacts reproduce exactly");
        }
    }

    Ok(())
}

fn parse_mode() -> Result<Mode> {
    let mut args = env::args().skip(1);
    let mode = match args.next().as_deref() {
        Some("generate") => Mode::Generate,
        Some("check" | "--check") => Mode::Check,
        Some("-h" | "--help") => {
            println!(
                "Usage: ddb-api-codegen <generate|--check>\n\n\
                 generate  rewrite checked-in artifacts when their bytes changed\n\
                 --check   fail when checked-in artifacts do not reproduce"
            );
            std::process::exit(0);
        }
        Some(other) => bail!("unknown mode {other}; expected generate or --check"),
        None => bail!("missing mode; expected generate or --check"),
    };

    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra}");
    }

    Ok(mode)
}

fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("api-codegen must live at <workspace>/tools/api-codegen")
}

fn generate_into_temp(workspace: &Path) -> Result<tempfile::TempDir> {
    let proto_root = workspace.join("proto");
    let proto_files = PROTO_FILES
        .iter()
        .map(|file| proto_root.join(file))
        .collect::<Vec<_>>();

    for path in &proto_files {
        if !path.is_file() {
            bail!("missing canonical schema {}", path.display());
        }
    }

    let output = tempfile::tempdir().context("create temporary codegen directory")?;
    let descriptor_path = output.path().join("ddb_api_v2_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .out_dir(output.path())
        .file_descriptor_set_path(&descriptor_path)
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .extern_path(".google.protobuf.Timestamp", "crate::wkt::Timestamp")
        .extern_path(".google.protobuf.FieldMask", "crate::wkt::FieldMask");

    let include_paths = protobuf_include_paths(&proto_root);
    config
        .compile_protos(&proto_files, &include_paths)
        .context("compile canonical Protobuf schema")?;

    let grpc_output = output.path().join("grpc");
    fs::create_dir_all(&grpc_output).context("create temporary gRPC output directory")?;
    tonic_prost_build::configure()
        .out_dir(&grpc_output)
        .build_client(true)
        .build_server(true)
        .emit_rerun_if_changed(false)
        .extern_path(".ddb.api.v2", "::ddb_api_types::v2")
        .compile_protos(&proto_files, &include_paths)
        .context("generate Tonic client and server stubs")?;

    let descriptors = fs::read(&descriptor_path)
        .with_context(|| format!("read {}", descriptor_path.display()))?;
    let descriptor_set = FileDescriptorSet::decode(descriptors.as_slice())
        .context("decode generated descriptor set for public specifications")?;
    let descriptors = strip_source_info(&descriptors)?;
    fs::write(&descriptor_path, &descriptors)
        .with_context(|| format!("write stripped descriptor {}", descriptor_path.display()))?;

    let mut json = pbjson_build::Builder::new();
    json.out_dir(output.path())
        .extern_path(".google.protobuf", "::pbjson_types")
        .extern_path(".google.protobuf.Timestamp", "crate::wkt::Timestamp")
        .extern_path(".google.protobuf.FieldMask", "crate::wkt::FieldMask")
        .ignore_unknown_fields()
        .ignore_unknown_enum_variants()
        .register_descriptors(&descriptors)
        .context("register descriptors for ProtoJSON generation")?
        .build(&[PACKAGE])
        .context("generate ProtoJSON implementations")?;

    for name in ["ddb.api.v2.rs", "ddb.api.v2.serde.rs"] {
        let path = output.path().join(name);
        if !path.is_file() {
            bail!("generator did not produce {}", path.display());
        }
    }
    if !grpc_output.join("ddb.api.v2.rs").is_file() {
        bail!("generator did not produce gRPC service stubs");
    }

    generate_public_specs(&descriptor_set, output.path())?;
    generate_language_contracts(&descriptor_set, output.path())?;

    for relative in [
        "ddb.api.v2.rs",
        "ddb.api.v2.serde.rs",
        "grpc/ddb.api.v2.rs",
        "openapi-v2.json",
        "asyncapi-v2.json",
        "typescript/types.ts",
        "typescript/contract.ts",
        "python/types.py",
        "python/contract.py",
    ] {
        normalize_generated_text(&output.path().join(relative))?;
    }

    Ok(output)
}

fn normalize_generated_text(path: &Path) -> Result<()> {
    let generated = fs::read_to_string(path)
        .with_context(|| format!("read generated text {}", path.display()))?;
    let mut lines = generated.lines().map(str::trim_end).collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let normalized = lines.join("\n");
    fs::write(path, format!("{normalized}\n"))
        .with_context(|| format!("normalize generated text {}", path.display()))
}

struct SchemaIndex<'a> {
    messages: BTreeMap<String, &'a DescriptorProto>,
    enums: BTreeMap<String, &'a EnumDescriptorProto>,
}

impl<'a> SchemaIndex<'a> {
    fn from_descriptor_set(descriptor_set: &'a FileDescriptorSet) -> Self {
        let mut index = Self {
            messages: BTreeMap::new(),
            enums: BTreeMap::new(),
        };
        for file in api_files(descriptor_set) {
            let package = file.package();
            index_messages(package, "", &file.message_type, &mut index);
            for descriptor in &file.enum_type {
                let full_name = format!(".{package}.{}", descriptor.name());
                index.enums.insert(full_name, descriptor);
            }
        }
        index
    }
}

fn index_messages<'a>(
    package: &str,
    parent: &str,
    descriptors: &'a [DescriptorProto],
    index: &mut SchemaIndex<'a>,
) {
    for descriptor in descriptors {
        let relative_name = if parent.is_empty() {
            descriptor.name().to_string()
        } else {
            format!("{parent}.{}", descriptor.name())
        };
        let full_name = format!(".{package}.{relative_name}");
        index.messages.insert(full_name.clone(), descriptor);
        for enum_descriptor in &descriptor.enum_type {
            index.enums.insert(
                format!("{full_name}.{}", enum_descriptor.name()),
                enum_descriptor,
            );
        }
        index_messages(package, &relative_name, &descriptor.nested_type, index);
    }
}

fn generate_public_specs(descriptor_set: &FileDescriptorSet, output: &Path) -> Result<()> {
    let index = SchemaIndex::from_descriptor_set(descriptor_set);
    let schemas = generate_json_schemas(&index)?;
    let openapi = build_openapi(descriptor_set, &schemas)?;
    let asyncapi = build_asyncapi(descriptor_set, &schemas)?;

    validate_local_references(&openapi, "OpenAPI")?;
    validate_local_references(&asyncapi, "AsyncAPI")?;
    validate_spec_coverage(descriptor_set, &openapi, &asyncapi)?;
    write_json(&output.join("openapi-v2.json"), &openapi)?;
    write_json(&output.join("asyncapi-v2.json"), &asyncapi)
}

fn generate_json_schemas(index: &SchemaIndex<'_>) -> Result<BTreeMap<String, Value>> {
    let mut schemas = BTreeMap::new();
    for (full_name, descriptor) in &index.enums {
        if let Some(name) = component_name(full_name) {
            let values = descriptor
                .value
                .iter()
                .map(|value| Value::String(value.name().to_string()))
                .collect::<Vec<_>>();
            schemas.insert(
                name.to_string(),
                json!({
                    "type": "string",
                    "description": "Extensible Protobuf enum. Clients must tolerate values added by newer servers.",
                    "x-extensible-enum": values,
                }),
            );
        }
    }
    for (full_name, descriptor) in &index.messages {
        if descriptor
            .options
            .as_ref()
            .is_some_and(|options| options.map_entry())
        {
            continue;
        }
        let Some(name) = component_name(full_name) else {
            continue;
        };
        let mut properties = Map::new();
        let mut oneofs = BTreeMap::<String, Vec<String>>::new();
        for field in &descriptor.field {
            let json_name = field_json_name(field);
            properties.insert(json_name.clone(), field_schema(field, index)?);
            if let Some(oneof_index) = field.oneof_index {
                if !field.proto3_optional() {
                    if let Some(oneof) = descriptor.oneof_decl.get(oneof_index as usize) {
                        oneofs
                            .entry(oneof.name().to_string())
                            .or_default()
                            .push(json_name);
                    }
                }
            }
        }
        let mut schema = json!({
            "type": "object",
            "properties": Value::Object(properties),
            "additionalProperties": true,
        });
        if !oneofs.is_empty() {
            schema["x-protobuf-oneofs"] = serde_json::to_value(oneofs)?;
        }
        schemas.insert(name.to_string(), schema);
    }
    Ok(schemas)
}

fn field_schema(field: &FieldDescriptorProto, index: &SchemaIndex<'_>) -> Result<Value> {
    if field.label() == Label::Repeated && field.r#type() == Type::Message {
        if let Some(entry) = index.messages.get(field.type_name()) {
            if entry
                .options
                .as_ref()
                .is_some_and(|options| options.map_entry())
            {
                let value = entry
                    .field
                    .iter()
                    .find(|candidate| candidate.name() == "value")
                    .context("Protobuf map entry has no value field")?;
                return Ok(json!({
                    "type": "object",
                    "additionalProperties": scalar_field_schema(value),
                }));
            }
        }
    }

    let schema = scalar_field_schema(field);
    if field.label() == Label::Repeated {
        Ok(json!({"type": "array", "items": schema}))
    } else {
        Ok(schema)
    }
}

fn scalar_field_schema(field: &FieldDescriptorProto) -> Value {
    match field.r#type() {
        Type::Double => json!({"type": "number", "format": "double"}),
        Type::Float => json!({"type": "number", "format": "float"}),
        Type::Int64 | Type::Sint64 | Type::Sfixed64 => json!({
            "type": "string",
            "format": "int64",
            "pattern": "^-?[0-9]+$",
        }),
        Type::Uint64 | Type::Fixed64 => json!({
            "type": "string",
            "format": "uint64",
            "pattern": "^[0-9]+$",
        }),
        Type::Int32 | Type::Sint32 | Type::Sfixed32 => {
            json!({"type": "integer", "format": "int32"})
        }
        Type::Uint32 | Type::Fixed32 => json!({
            "type": "integer",
            "format": "int64",
            "minimum": 0,
            "maximum": 4_294_967_295_u64,
        }),
        Type::Bool => json!({"type": "boolean"}),
        Type::String => json!({"type": "string"}),
        Type::Bytes => json!({"type": "string", "format": "byte"}),
        Type::Enum | Type::Message | Type::Group => schema_reference(field.type_name()),
    }
}

fn schema_reference(full_name: &str) -> Value {
    match full_name {
        ".google.protobuf.Timestamp" => json!({"type": "string", "format": "date-time"}),
        ".google.protobuf.Duration" => json!({
            "type": "string",
            "pattern": "^-?[0-9]+(?:\\.[0-9]{1,9})?s$",
        }),
        ".google.protobuf.FieldMask" => json!({
            "type": "string",
            "format": "field-mask",
            "pattern": "^(?:[A-Za-z0-9_.]+(?:,[A-Za-z0-9_.]+)*)?$",
        }),
        name => {
            let component = component_name(name).unwrap_or_else(|| {
                name.trim_start_matches('.')
                    .rsplit('.')
                    .next()
                    .unwrap_or(name)
            });
            json!({"$ref": format!("#/components/schemas/{component}")})
        }
    }
}

fn component_name(full_name: &str) -> Option<&str> {
    full_name
        .strip_prefix(PACKAGE)
        .map(|name| name.trim_start_matches('.'))
        .filter(|name| !name.is_empty())
}

fn field_json_name(field: &FieldDescriptorProto) -> String {
    field
        .json_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| field.name())
        .to_string()
}

fn generate_language_contracts(descriptors: &FileDescriptorSet, output: &Path) -> Result<()> {
    let index = SchemaIndex::from_descriptor_set(descriptors);
    let typescript = output.join("typescript");
    let python = output.join("python");
    fs::create_dir_all(&typescript).context("create TypeScript codegen directory")?;
    fs::create_dir_all(&python).context("create Python codegen directory")?;
    fs::write(typescript.join("types.ts"), typescript_types(&index)?)
        .context("write generated TypeScript types")?;
    fs::write(
        typescript.join("contract.ts"),
        typescript_contract(descriptors, &index)?,
    )
    .context("write generated TypeScript contract")?;
    fs::write(python.join("types.py"), python_types(&index)?)
        .context("write generated Python types")?;
    fs::write(
        python.join("contract.py"),
        python_contract(descriptors, &index)?,
    )
    .context("write generated Python contract")?;
    Ok(())
}

fn language_name(full_name: &str) -> String {
    component_name(full_name)
        .unwrap_or_else(|| {
            full_name
                .trim_start_matches('.')
                .rsplit('.')
                .next()
                .unwrap_or(full_name)
        })
        .replace('.', "_")
}

fn map_value_field<'a>(
    field: &FieldDescriptorProto,
    index: &'a SchemaIndex<'_>,
) -> Option<&'a FieldDescriptorProto> {
    if field.label() != Label::Repeated || field.r#type() != Type::Message {
        return None;
    }
    index
        .messages
        .get(field.type_name())
        .filter(|descriptor| {
            descriptor
                .options
                .as_ref()
                .is_some_and(|options| options.map_entry())
        })
        .and_then(|descriptor| {
            descriptor
                .field
                .iter()
                .find(|field| field.name() == "value")
        })
}

fn typescript_scalar(field: &FieldDescriptorProto) -> String {
    match field.r#type() {
        Type::Double | Type::Float => "number".to_string(),
        Type::Int64 | Type::Sint64 | Type::Sfixed64 | Type::Uint64 | Type::Fixed64 => {
            "string".to_string()
        }
        Type::Int32 | Type::Sint32 | Type::Sfixed32 | Type::Uint32 | Type::Fixed32 => {
            "number".to_string()
        }
        Type::Bool => "boolean".to_string(),
        Type::String | Type::Bytes => "string".to_string(),
        Type::Enum | Type::Message | Type::Group => match field.type_name() {
            ".google.protobuf.Timestamp"
            | ".google.protobuf.Duration"
            | ".google.protobuf.FieldMask" => "string".to_string(),
            name => language_name(name),
        },
    }
}

fn typescript_field(field: &FieldDescriptorProto, index: &SchemaIndex<'_>) -> String {
    if let Some(value) = map_value_field(field, index) {
        return format!("Record<string, {}>", typescript_scalar(value));
    }
    let scalar = typescript_scalar(field);
    if field.label() == Label::Repeated {
        format!("{scalar}[]")
    } else {
        scalar
    }
}

fn typescript_types(index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(
        "// @generated by ddb-api-codegen from the canonical Protobuf schema.\n\
         // Do not edit. int64/uint64 are decimal strings; bytes are base64 strings.\n\n",
    );
    for (full_name, descriptor) in &index.enums {
        let name = language_name(full_name);
        writeln!(output, "export const {name}Values = {{")?;
        for value in &descriptor.value {
            writeln!(
                output,
                "  {}: {},",
                value.name(),
                serde_json::to_string(value.name())?
            )?;
        }
        writeln!(output, "}} as const;")?;
        writeln!(
            output,
            "export type {name} = (typeof {name}Values)[keyof typeof {name}Values] | (string & {{}});\n"
        )?;
    }
    for (full_name, descriptor) in &index.messages {
        if descriptor
            .options
            .as_ref()
            .is_some_and(|options| options.map_entry())
        {
            continue;
        }
        writeln!(output, "export interface {} {{", language_name(full_name))?;
        for field in &descriptor.field {
            writeln!(
                output,
                "  {}?: {};",
                serde_json::to_string(&field_json_name(field))?,
                typescript_field(field, index)
            )?;
        }
        output.push_str("}\n\n");
    }
    Ok(output)
}

fn paginated_item<'a>(
    input: &DescriptorProto,
    output: &'a DescriptorProto,
    index: &SchemaIndex<'_>,
) -> Option<&'a FieldDescriptorProto> {
    let accepts_page = input
        .field
        .iter()
        .any(|field| field.name() == "page" && field.type_name().ends_with(".PageRequest"));
    let returns_page = output
        .field
        .iter()
        .any(|field| field.name() == "page" && field.type_name().ends_with(".PageInfo"));
    if !accepts_page || !returns_page {
        return None;
    }
    let mut candidates = output.field.iter().filter(|field| {
        field.label() == Label::Repeated && map_value_field(field, index).is_none()
    });
    let item = candidates.next()?;
    candidates.next().is_none().then_some(item)
}

fn typescript_contract(descriptors: &FileDescriptorSet, index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(
        "// @generated by ddb-api-codegen from the canonical Protobuf schema.\n\
         // Do not edit.\n\nimport type * as t from \"./types.js\";\n\n\
         export interface MethodMap {\n",
    );
    for file in api_files(descriptors) {
        for service in &file.service {
            for method in &service.method {
                let key = format!("{}.{}", service.name(), method.name());
                let request = language_name(method.input_type());
                let response = language_name(method.output_type());
                let scope = required_scope(service.name(), method.name())?
                    .map(|scope| format!("\"{scope}\""))
                    .unwrap_or_else(|| "null".to_string());
                writeln!(
                    output,
                    "  \"{key}\": {{ request: t.{request}; response: t.{response}; serverStreaming: {}; scope: {scope} }};",
                    method.server_streaming()
                )?;
            }
        }
    }
    output.push_str(
        "}\n\n\
         export type MethodName = keyof MethodMap;\n\
         export type UnaryMethodName = { [K in MethodName]: MethodMap[K][\"serverStreaming\"] extends false ? K : never }[MethodName];\n\
         export type StreamingMethodName = { [K in MethodName]: MethodMap[K][\"serverStreaming\"] extends true ? K : never }[MethodName];\n\
         export type RequestOf<K extends MethodName> = MethodMap[K][\"request\"];\n\
         export type ResponseOf<K extends MethodName> = MethodMap[K][\"response\"];\n\n\
         export interface MethodSpec { readonly path: string; readonly serverStreaming: boolean; readonly scope: \"read\" | \"control\" | \"admin\" | null; }\n\n\
         export const METHODS = {\n",
    );
    for file in api_files(descriptors) {
        for service in &file.service {
            let full_service = format!("{}.{}", file.package(), service.name());
            for method in &service.method {
                let key = format!("{}.{}", service.name(), method.name());
                let path = format!("/api/v2/rpc/{full_service}/{}", method.name());
                let scope = required_scope(service.name(), method.name())?
                    .map(|scope| format!("\"{scope}\""))
                    .unwrap_or_else(|| "null".to_string());
                writeln!(
                    output,
                    "  \"{key}\": {{ path: \"{path}\", serverStreaming: {}, scope: {scope} }},",
                    method.server_streaming()
                )?;
            }
        }
    }
    output.push_str("} as const satisfies Record<MethodName, MethodSpec>;\n\n");
    output.push_str("export interface PaginatedMethodMap {\n");
    for file in api_files(descriptors) {
        for service in &file.service {
            for method in &service.method {
                let input = index
                    .messages
                    .get(method.input_type())
                    .context("public method input descriptor is missing")?;
                let response = index
                    .messages
                    .get(method.output_type())
                    .context("public method output descriptor is missing")?;
                let Some(item) = paginated_item(input, response, index) else {
                    continue;
                };
                let key = format!("{}.{}", service.name(), method.name());
                writeln!(
                    output,
                    "  \"{key}\": {{ item: t.{}; itemsField: \"{}\" }};",
                    typescript_scalar(item),
                    field_json_name(item)
                )?;
            }
        }
    }
    output.push_str(
        "}\n\n\
         export type PaginatedMethodName = keyof PaginatedMethodMap;\n\
         export type PaginatedItemOf<K extends PaginatedMethodName> = PaginatedMethodMap[K][\"item\"];\n\n\
         export const PAGINATED_METHODS = {\n",
    );
    for file in api_files(descriptors) {
        for service in &file.service {
            for method in &service.method {
                let input = index.messages.get(method.input_type()).unwrap();
                let response = index.messages.get(method.output_type()).unwrap();
                let Some(item) = paginated_item(input, response, index) else {
                    continue;
                };
                let key = format!("{}.{}", service.name(), method.name());
                writeln!(
                    output,
                    "  \"{key}\": {{ itemsField: \"{}\" }},",
                    field_json_name(item)
                )?;
            }
        }
    }
    output.push_str("} as const;\n");
    Ok(output)
}

fn python_scalar(field: &FieldDescriptorProto) -> String {
    match field.r#type() {
        Type::Double | Type::Float => "float".to_string(),
        Type::Int64 | Type::Sint64 | Type::Sfixed64 | Type::Uint64 | Type::Fixed64 => {
            "str".to_string()
        }
        Type::Int32 | Type::Sint32 | Type::Sfixed32 | Type::Uint32 | Type::Fixed32 => {
            "int".to_string()
        }
        Type::Bool => "bool".to_string(),
        Type::String | Type::Bytes => "str".to_string(),
        Type::Enum | Type::Message | Type::Group => match field.type_name() {
            ".google.protobuf.Timestamp"
            | ".google.protobuf.Duration"
            | ".google.protobuf.FieldMask" => "str".to_string(),
            name => language_name(name),
        },
    }
}

fn python_field(field: &FieldDescriptorProto, index: &SchemaIndex<'_>) -> String {
    if let Some(value) = map_value_field(field, index) {
        return format!("dict[str, {}]", python_scalar(value));
    }
    let scalar = python_scalar(field);
    if field.label() == Label::Repeated {
        format!("list[{scalar}]")
    } else {
        scalar
    }
}

fn python_types(index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(
        "# @generated by ddb-api-codegen from the canonical Protobuf schema.\n\
         # Do not edit. ProtoJSON int64/uint64 are decimal str; bytes are base64 str.\n\n\
         from __future__ import annotations\n\n\
         from typing import NotRequired, TypeAlias, TypedDict\n\n",
    );
    for (full_name, descriptor) in &index.enums {
        let name = language_name(full_name);
        writeln!(output, "{name}: TypeAlias = str")?;
        writeln!(output, "class {name}Values:")?;
        for value in &descriptor.value {
            writeln!(
                output,
                "    {} = {}",
                value.name(),
                serde_json::to_string(value.name())?
            )?;
        }
        output.push('\n');
    }
    for (full_name, descriptor) in &index.messages {
        if descriptor
            .options
            .as_ref()
            .is_some_and(|options| options.map_entry())
        {
            continue;
        }
        let name = language_name(full_name);
        writeln!(output, "{name} = TypedDict(")?;
        writeln!(output, "    \"{name}\",")?;
        output.push_str("    {\n");
        for field in &descriptor.field {
            writeln!(
                output,
                "        {}: NotRequired[{}],",
                serde_json::to_string(&field_json_name(field))?,
                serde_json::to_string(&python_field(field, index))?
            )?;
        }
        output.push_str("    },\n)\n\n");
    }
    Ok(output)
}

fn python_contract(descriptors: &FileDescriptorSet, index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(concat!(
        "# @generated by ddb-api-codegen from the canonical Protobuf schema.\n",
        "# Do not edit.\n\n",
        "from __future__ import annotations\n\n",
        "from dataclasses import dataclass\n\n\n",
        "@dataclass(frozen=True, slots=True)\n",
        "class MethodSpec:\n",
        "    path: str\n",
        "    server_streaming: bool\n",
        "    scope: str | None\n\n\n",
        "METHODS: dict[str, MethodSpec] = {\n",
    ));
    for file in api_files(descriptors) {
        for service in &file.service {
            let full_service = format!("{}.{}", file.package(), service.name());
            for method in &service.method {
                let key = format!("{}.{}", service.name(), method.name());
                let path = format!("/api/v2/rpc/{full_service}/{}", method.name());
                let scope = required_scope(service.name(), method.name())?
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_else(|| "None".to_string());
                writeln!(
                    output,
                    "    \"{key}\": MethodSpec(\"{path}\", {}, {scope}),",
                    if method.server_streaming() {
                        "True"
                    } else {
                        "False"
                    }
                )?;
            }
        }
    }
    output.push_str("}\n\nPAGINATED_METHODS: dict[str, str] = {\n");
    for file in api_files(descriptors) {
        for service in &file.service {
            for method in &service.method {
                let input = index
                    .messages
                    .get(method.input_type())
                    .context("public method input descriptor is missing")?;
                let response = index
                    .messages
                    .get(method.output_type())
                    .context("public method output descriptor is missing")?;
                let Some(item) = paginated_item(input, response, index) else {
                    continue;
                };
                writeln!(
                    output,
                    "    \"{}.{}\": \"{}\",",
                    service.name(),
                    method.name(),
                    field_json_name(item)
                )?;
            }
        }
    }
    output.push_str("}\n");
    Ok(output)
}

fn build_openapi(
    descriptor_set: &FileDescriptorSet,
    schemas: &BTreeMap<String, Value>,
) -> Result<Value> {
    let mut paths = Map::new();
    let mut tags = BTreeMap::<String, Value>::new();
    for file in api_files(descriptor_set) {
        for service in &file.service {
            let service_name = service.name();
            let full_service = format!("{}.{}", file.package(), service_name);
            tags.insert(
                service_name.to_string(),
                json!({
                    "name": service_name,
                    "description": service_description(service_name),
                }),
            );
            for method in &service.method {
                let method_name = method.name();
                let path = format!("/api/v2/rpc/{full_service}/{method_name}");
                let output_schema = schema_reference(method.output_type());
                let success_content = if method.server_streaming() {
                    json!({
                        "application/x-ndjson": {
                            "schema": {
                                "type": "string",
                                "format": "ndjson",
                                "description": "One canonical ProtoJSON message per non-empty line. Empty lines are transport heartbeats."
                            },
                            "x-ddb-stream-message": output_schema,
                        }
                    })
                } else {
                    json!({"application/json": {"schema": output_schema}})
                };
                let mut operation = json!({
                    "operationId": format!("{service_name}_{method_name}"),
                    "summary": format!("{service_name}.{method_name}"),
                    "tags": [service_name],
                    "x-ddb-protobuf-method": format!("/{full_service}/{method_name}"),
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": schema_reference(method.input_type())
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": if method.server_streaming() { "Streaming response" } else { "Successful response" },
                            "content": success_content
                        },
                        "400": {
                            "description": "Invalid request or malformed ProtoJSON",
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/DdbError"}
                                }
                            }
                        },
                        "default": {
                            "description": "Stable DDB error",
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/DdbError"}
                                }
                            }
                        }
                    }
                });
                if let Some(scope) = required_scope(service_name, method_name)? {
                    operation["security"] = json!([{"bearerAuth": []}]);
                    operation["x-ddb-required-scope"] = Value::String(scope.to_string());
                } else {
                    operation["security"] = json!([]);
                }
                paths.insert(path, json!({"post": operation}));
            }
        }
    }

    let schemas = schemas
        .iter()
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect::<Map<_, _>>();
    Ok(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "DDB API v2",
            "version": "2.0.0-draft.3",
            "description": "Backend-neutral debugger API. Protobuf is canonical; this document describes the HTTP/ProtoJSON binding.",
            "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"}
        },
        "servers": [{"url": "/"}],
        "paths": Value::Object(paths),
        "tags": tags.into_values().collect::<Vec<_>>(),
        "components": {
            "schemas": Value::Object(schemas),
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Configured DDB bearer token. Effective authentication mode is discoverable through GetCapabilities."
                }
            }
        }
    }))
}

fn build_asyncapi(
    descriptor_set: &FileDescriptorSet,
    schemas: &BTreeMap<String, Value>,
) -> Result<Value> {
    let mut channels = Map::new();
    let mut operations = Map::new();
    let mut messages = Map::new();
    for file in api_files(descriptor_set) {
        for service in &file.service {
            let service_name = service.name();
            let full_service = format!("{}.{}", file.package(), service_name);
            for method in &service.method {
                if !method.server_streaming() {
                    continue;
                }
                let method_name = method.name();
                let channel_name = lower_camel(method_name);
                let message_name = component_name(method.output_type())
                    .context("stream output must be an API component")?;
                messages.entry(message_name.to_string()).or_insert_with(|| {
                    json!({
                        "name": message_name,
                        "title": message_name,
                        "contentType": "application/json",
                        "payload": schema_reference(method.output_type())
                    })
                });
                channels.insert(
                    channel_name.clone(),
                    json!({
                        "address": format!("/api/v2/rpc/{full_service}/{method_name}"),
                        "description": format!("{service_name}.{method_name} NDJSON stream"),
                        "messages": {
                            message_name: {"$ref": format!("#/components/messages/{message_name}")}
                        },
                        "x-ddb-request-schema": schema_reference(method.input_type()),
                        "x-ddb-heartbeat": "An empty line is emitted after 15 seconds without an event."
                    }),
                );
                let mut operation = json!({
                    "action": "send",
                    "channel": {"$ref": format!("#/channels/{channel_name}")},
                    "messages": [{"$ref": format!("#/channels/{channel_name}/messages/{message_name}")}],
                    "bindings": {
                        "http": {
                            "method": "POST",
                            "bindingVersion": "0.3.0"
                        }
                    }
                });
                if let Some(scope) = required_scope(service_name, method_name)? {
                    operation["x-ddb-required-scope"] = Value::String(scope.to_string());
                }
                operations.insert(format!("send{method_name}"), operation);
            }
        }
    }

    let schemas = schemas
        .iter()
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect::<Map<_, _>>();
    Ok(json!({
        "asyncapi": "3.1.0",
        "info": {
            "title": "DDB API v2 event streams",
            "version": "2.0.0-draft.3",
            "description": "Replayable state and independent debugger output streams over HTTP NDJSON."
        },
        "defaultContentType": "application/json",
        "servers": {
            "local": {
                "host": "127.0.0.1:8080",
                "protocol": "http",
                "description": "Default local endpoint; use GetCapabilities for the effective endpoint."
            }
        },
        "channels": Value::Object(channels),
        "operations": Value::Object(operations),
        "components": {
            "schemas": Value::Object(schemas),
            "messages": Value::Object(messages),
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Configured DDB bearer token."
                }
            }
        }
    }))
}

fn required_scope(service: &str, method: &str) -> Result<Option<&'static str>> {
    Ok(match (service, method) {
        ("DebuggerService", "GetServerInfo")
        | ("DdbAdminService", "GetHealth" | "GetReadiness") => None,
        ("DebuggerService", "ReadMemory") => Some("control"),
        ("DebuggerService" | "DdbEventService", _) => Some("read"),
        ("DebuggerControlService", _) => Some("control"),
        ("DdbAdminService", _) => Some("admin"),
        _ => bail!("public RPC {service}.{method} has no authorization classification"),
    })
}

fn service_description(service: &str) -> &'static str {
    match service {
        "DebuggerService" => {
            "Debugger metadata, topology, inspection, source, and operation reads."
        }
        "DebuggerControlService" => "Idempotently admitted debugger and DDB control operations.",
        "DdbEventService" => "Replayable state and independent debugger output streams.",
        "DdbAdminService" => "Health, readiness, and privileged lifecycle operations.",
        _ => "DDB API service.",
    }
}

fn lower_camel(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

fn api_files(
    descriptor_set: &FileDescriptorSet,
) -> impl Iterator<Item = &prost_types::FileDescriptorProto> {
    descriptor_set
        .file
        .iter()
        .filter(|file| file.package() == PACKAGE.trim_start_matches('.'))
}

fn validate_local_references(document: &Value, label: &str) -> Result<()> {
    fn visit(document: &Value, value: &Value, label: &str) -> Result<()> {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    if let Some(pointer) = reference.strip_prefix('#') {
                        if document.pointer(pointer).is_none() {
                            bail!("{label} contains unresolved local reference {reference}");
                        }
                    }
                }
                for child in object.values() {
                    visit(document, child, label)?;
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(document, child, label)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(document, document, label)
}

fn validate_spec_coverage(
    descriptor_set: &FileDescriptorSet,
    openapi: &Value,
    asyncapi: &Value,
) -> Result<()> {
    for file in api_files(descriptor_set) {
        for service in &file.service {
            let full_service = format!("{}.{}", file.package(), service.name());
            for method in &service.method {
                let path = format!("/api/v2/rpc/{full_service}/{}", method.name());
                if openapi["paths"].get(&path).is_none() {
                    bail!(
                        "OpenAPI omitted public method {full_service}.{}",
                        method.name()
                    );
                }
                let channel = lower_camel(method.name());
                if method.server_streaming() != asyncapi["channels"].get(&channel).is_some() {
                    bail!(
                        "AsyncAPI stream coverage disagrees for {full_service}.{}",
                        method.name()
                    );
                }
            }
        }
    }
    Ok(())
}

fn write_json(path: &Path, document: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document).context("serialize public API spec")?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn strip_source_info(descriptors: &[u8]) -> Result<Vec<u8>> {
    let mut descriptor_set = prost_types::FileDescriptorSet::decode(descriptors)
        .context("decode generated descriptor set")?;
    for file in &mut descriptor_set.file {
        file.source_code_info = None;
    }
    Ok(descriptor_set.encode_to_vec())
}

fn protobuf_include_paths(proto_root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![proto_root.to_path_buf()];

    if let Some(path) = env::var_os("PROTOC_INCLUDE") {
        paths.push(PathBuf::from(path));
        return paths;
    }

    for candidate in ["/usr/include", "/usr/local/include"] {
        let path = PathBuf::from(candidate);
        if path.join("google/protobuf/timestamp.proto").is_file() {
            paths.push(path);
            break;
        }
    }

    paths
}

fn files_equal(left: &Path, right: &Path) -> Result<bool> {
    if !right.is_file() {
        return Ok(false);
    }

    let left_bytes = fs::read(left).with_context(|| format!("read {}", left.display()))?;
    let right_bytes = fs::read(right).with_context(|| format!("read {}", right.display()))?;
    Ok(left_bytes == right_bytes)
}

fn copy_if_changed(source: &Path, destination: &Path) -> Result<()> {
    if files_equal(source, destination)? {
        return Ok(());
    }

    let parent = destination
        .parent()
        .with_context(|| format!("destination {} has no parent", destination.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    fs::copy(source, destination).with_context(|| {
        format!(
            "copy generated artifact {} to {}",
            source.display(),
            destination.display()
        )
    })?;

    println!("updated {}", destination.display());
    Ok(())
}

fn display_relative<'a>(workspace: &'a Path, path: &'a Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .display()
        .to_string()
}
