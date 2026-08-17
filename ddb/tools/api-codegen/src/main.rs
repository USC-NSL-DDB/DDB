//! Deterministic generator and drift checker for the public DDB API contract.

mod registry;

use registry::OperationRegistry;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
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
            generated.path().join("operation-registry-v2.json"),
            workspace.join("docs/api/generated/operation-registry-v2.json"),
        ),
        (
            generated.path().join("runtime/v2_contract.rs"),
            workspace.join("core/src/api/generated/v2_contract.rs"),
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

    let operation_registry = OperationRegistry::load(workspace, &descriptor_set)?;
    generate_public_specs(&descriptor_set, &operation_registry, output.path())?;
    generate_language_contracts(&descriptor_set, &operation_registry, output.path())?;
    write_json(
        &output.path().join("operation-registry-v2.json"),
        &operation_registry.document(),
    )?;
    let runtime_output = output.path().join("runtime");
    fs::create_dir_all(&runtime_output).context("create runtime contract output directory")?;
    fs::write(
        runtime_output.join("v2_contract.rs"),
        operation_registry.runtime_source()?,
    )
    .context("write generated v2 runtime contract")?;
    rustfmt_generated(&runtime_output.join("v2_contract.rs"))?;

    for relative in [
        "ddb.api.v2.rs",
        "ddb.api.v2.serde.rs",
        "grpc/ddb.api.v2.rs",
        "openapi-v2.json",
        "asyncapi-v2.json",
        "operation-registry-v2.json",
        "runtime/v2_contract.rs",
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
fn rustfmt_generated(path: &Path) -> Result<()> {
    let rustfmt = env::var_os("RUSTFMT").unwrap_or_else(|| "rustfmt".into());
    let status = Command::new(&rustfmt)
        .arg("--edition")
        .arg("2021")
        .arg(path)
        .status()
        .with_context(|| format!("run {:?} for {}", rustfmt, path.display()))?;
    if !status.success() {
        bail!("rustfmt failed for {}", path.display());
    }
    Ok(())
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

fn generate_public_specs(
    descriptor_set: &FileDescriptorSet,
    registry: &OperationRegistry,
    output: &Path,
) -> Result<()> {
    let index = SchemaIndex::from_descriptor_set(descriptor_set);
    let schemas = generate_json_schemas(&index)?;
    let openapi = build_openapi(registry, &schemas)?;
    let asyncapi = build_asyncapi(registry, &schemas)?;

    validate_local_references(&openapi, "OpenAPI")?;
    validate_local_references(&asyncapi, "AsyncAPI")?;
    validate_spec_coverage(registry, &openapi, &asyncapi)?;
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

fn generate_language_contracts(
    descriptors: &FileDescriptorSet,
    registry: &OperationRegistry,
    output: &Path,
) -> Result<()> {
    let index = SchemaIndex::from_descriptor_set(descriptors);
    let typescript = output.join("typescript");
    let python = output.join("python");
    fs::create_dir_all(&typescript).context("create TypeScript codegen directory")?;
    fs::create_dir_all(&python).context("create Python codegen directory")?;
    fs::write(typescript.join("types.ts"), typescript_types(&index)?)
        .context("write generated TypeScript types")?;
    fs::write(
        typescript.join("contract.ts"),
        typescript_contract(registry, &index)?,
    )
    .context("write generated TypeScript contract")?;
    fs::write(python.join("types.py"), python_types(&index)?)
        .context("write generated Python types")?;
    fs::write(
        python.join("contract.py"),
        python_contract(registry, &index)?,
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

fn typescript_contract(registry: &OperationRegistry, index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(
        "// @generated by ddb-api-codegen from the canonical operation registry.\n\
         // Do not edit.\n\nimport type * as t from \"./types.js\";\n\n\
         export interface MethodMap {\n",
    );
    for operation in &registry.operations {
        let request = language_name(&operation.input_type);
        let response = language_name(&operation.output_type);
        let scope = sdk_scope(operation.permission.as_str())?;
        writeln!(
            output,
            "  \"{}\": {{ request: t.{request}; response: t.{response}; serverStreaming: {}; scope: {scope} }};",
            operation.key,
            operation.server_streaming
        )?;
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
    for operation in &registry.operations {
        let scope = sdk_scope(operation.permission.as_str())?;
        writeln!(
            output,
            "  \"{}\": {{ path: {}, serverStreaming: {}, scope: {scope} }},",
            operation.key,
            serde_json::to_string(&operation.path)?,
            operation.server_streaming
        )?;
    }
    output.push_str("} as const satisfies Record<MethodName, MethodSpec>;\n\n");
    output.push_str("export interface PaginatedMethodMap {\n");
    for operation in &registry.operations {
        let input = index
            .messages
            .get(&operation.input_type)
            .context("public method input descriptor is missing")?;
        let response = index
            .messages
            .get(&operation.output_type)
            .context("public method output descriptor is missing")?;
        let Some(item) = paginated_item(input, response, index) else {
            continue;
        };
        writeln!(
            output,
            "  \"{}\": {{ item: t.{}; itemsField: \"{}\" }};",
            operation.key,
            typescript_scalar(item),
            field_json_name(item)
        )?;
    }
    output.push_str(
        "}\n\n\
         export type PaginatedMethodName = keyof PaginatedMethodMap;\n\
         export type PaginatedItemOf<K extends PaginatedMethodName> = PaginatedMethodMap[K][\"item\"];\n\n\
         export const PAGINATED_METHODS = {\n",
    );
    for operation in &registry.operations {
        let input = index
            .messages
            .get(&operation.input_type)
            .context("public method input descriptor is missing")?;
        let response = index
            .messages
            .get(&operation.output_type)
            .context("public method output descriptor is missing")?;
        let Some(item) = paginated_item(input, response, index) else {
            continue;
        };
        writeln!(
            output,
            "  \"{}\": {{ itemsField: \"{}\" }},",
            operation.key,
            field_json_name(item)
        )?;
    }
    output.push_str("} as const;\n");
    Ok(output)
}

fn sdk_scope(permission: &str) -> Result<String> {
    match permission {
        "public" => Ok("null".to_string()),
        "read" | "control" | "admin" => Ok(serde_json::to_string(permission)?),
        other => bail!("unsupported SDK permission {other}"),
    }
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

fn python_contract(registry: &OperationRegistry, index: &SchemaIndex<'_>) -> Result<String> {
    let mut output = String::from(concat!(
        "# @generated by ddb-api-codegen from the canonical operation registry.\n",
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
    for operation in &registry.operations {
        let scope = match operation.permission.as_str() {
            "public" => "None".to_string(),
            permission => serde_json::to_string(permission)?,
        };
        writeln!(
            output,
            "    {}: MethodSpec({}, {}, {scope}),",
            serde_json::to_string(&operation.key)?,
            serde_json::to_string(&operation.path)?,
            if operation.server_streaming {
                "True"
            } else {
                "False"
            }
        )?;
    }
    output.push_str("}\n\nPAGINATED_METHODS: dict[str, str] = {\n");
    for operation in &registry.operations {
        let input = index
            .messages
            .get(&operation.input_type)
            .context("public method input descriptor is missing")?;
        let response = index
            .messages
            .get(&operation.output_type)
            .context("public method output descriptor is missing")?;
        let Some(item) = paginated_item(input, response, index) else {
            continue;
        };
        writeln!(
            output,
            "    {}: {},",
            serde_json::to_string(&operation.key)?,
            serde_json::to_string(&field_json_name(item))?
        )?;
    }
    output.push_str("}\n");
    Ok(output)
}

fn build_openapi(registry: &OperationRegistry, schemas: &BTreeMap<String, Value>) -> Result<Value> {
    let mut paths = Map::new();
    let tags = registry
        .services
        .iter()
        .map(|service| {
            json!({
                "name": service.name,
                "description": service.description,
                "x-ddb-protobuf-service": service.full_name,
            })
        })
        .collect::<Vec<_>>();

    for operation in &registry.operations {
        let output_schema = schema_reference(&operation.output_type);
        let response_content_type = if operation.server_streaming {
            &registry.http.stream_response_content_type
        } else {
            &registry.http.unary_response_content_type
        };
        let response_schema = if operation.server_streaming {
            json!({
                "type": "string",
                "format": "ndjson",
                "description": "One canonical ProtoJSON message per non-empty line. Empty lines are transport heartbeats.",
                "x-ddb-stream-message": output_schema,
            })
        } else {
            output_schema
        };
        let mut success_content = Map::new();
        success_content.insert(
            response_content_type.clone(),
            json!({"schema": response_schema}),
        );
        let mut responses = openapi_error_responses(registry);
        responses.insert(
            registry.http.success_status.to_string(),
            json!({
                "description": if operation.server_streaming {
                    "Streaming response"
                } else {
                    "Successful response"
                },
                "content": Value::Object(success_content),
            }),
        );

        let mut request_content = Map::new();
        request_content.insert(
            registry.http.request_content_type.clone(),
            json!({
                "schema": schema_reference(&operation.input_type),
                "example": {},
            }),
        );
        let mut contract_operation = json!({
            "operationId": operation.operation_id,
            "summary": operation.description,
            "description": operation.description,
            "tags": [operation.service],
            "x-ddb-registry-key": operation.key,
            "x-ddb-protobuf-method": operation.protobuf_method,
            "requestBody": {
                "required": true,
                "content": Value::Object(request_content),
            },
            "responses": Value::Object(responses),
        });
        if operation.permission.as_str() == "public" {
            contract_operation["security"] = json!([]);
        } else {
            contract_operation["security"] = json!([{"bearerAuth": []}]);
            contract_operation["x-ddb-required-scope"] =
                Value::String(operation.permission.as_str().to_string());
        }
        let mut path_item = Map::new();
        path_item.insert(registry.http.method.clone(), contract_operation);
        paths.insert(operation.path.clone(), Value::Object(path_item));
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
            "description": "Backend-neutral debugger API. Protobuf plus the checked operation policy form the canonical registry; this document is its HTTP/ProtoJSON projection.",
            "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"}
        },
        "servers": [{"url": "/"}],
        "paths": Value::Object(paths),
        "tags": tags,
        "x-ddb-operation-registry": "./operation-registry-v2.json",
        "x-ddb-max-request-bytes": registry.http.max_request_bytes,
        "components": {
            "schemas": Value::Object(schemas),
            "responses": Value::Object(openapi_error_components(registry)),
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

fn openapi_error_responses(registry: &OperationRegistry) -> Map<String, Value> {
    registry
        .errors
        .iter()
        .map(|error| error.status)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|status| {
            (
                status.to_string(),
                json!({"$ref": format!("#/components/responses/DdbError{status}")}),
            )
        })
        .collect()
}

fn openapi_error_components(registry: &OperationRegistry) -> Map<String, Value> {
    let mut grouped = BTreeMap::<u16, Vec<_>>::new();
    for error in &registry.errors {
        grouped.entry(error.status).or_default().push(error);
    }
    grouped
        .into_iter()
        .map(|(status, errors)| {
            let codes = errors
                .iter()
                .map(|error| Value::String(error.code.clone()))
                .collect::<Vec<_>>();
            let description = errors
                .iter()
                .map(|error| error.description.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (
                format!("DdbError{status}"),
                json!({
                    "description": description,
                    "x-ddb-error-codes": codes,
                    "content": {
                        registry.http.unary_response_content_type.clone(): {
                            "schema": {"$ref": "#/components/schemas/DdbError"}
                        }
                    }
                }),
            )
        })
        .collect()
}
fn build_asyncapi(
    registry: &OperationRegistry,
    schemas: &BTreeMap<String, Value>,
) -> Result<Value> {
    let mut channels = Map::new();
    let mut operations = Map::new();
    let mut messages = Map::new();

    for operation_spec in registry
        .operations
        .iter()
        .filter(|operation| operation.server_streaming)
    {
        let stream = operation_spec
            .stream
            .as_ref()
            .context("validated streaming operation has no stream policy")?;
        let message_name = component_name(&operation_spec.output_type)
            .context("stream output must be an API component")?;
        messages.entry(message_name.to_string()).or_insert_with(|| {
            json!({
                "name": message_name,
                "title": message_name,
                "contentType": registry.http.unary_response_content_type,
                "payload": schema_reference(&operation_spec.output_type),
                "examples": [{"name": "protoJsonEnvelope", "payload": {}}]
            })
        });
        channels.insert(
            stream.channel.clone(),
            json!({
                "address": operation_spec.path,
                "description": operation_spec.description,
                "messages": {
                    message_name: {"$ref": format!("#/components/messages/{message_name}")}
                },
                "x-ddb-registry-key": operation_spec.key,
                "x-ddb-protobuf-method": operation_spec.protobuf_method,
                "x-ddb-request-schema": schema_reference(&operation_spec.input_type),
                "x-ddb-lane": stream.lane,
                "x-ddb-heartbeat-seconds": stream.heartbeat_seconds,
                "x-ddb-heartbeat": format!(
                    "An empty line is emitted after {} seconds without an event.",
                    stream.heartbeat_seconds
                ),
                "x-ddb-cursor-replay": stream.cursor_replay,
                "x-ddb-ordering": stream.ordering,
                "x-ddb-replay-limits": stream.replay_limits,
                "x-ddb-backpressure": stream.backpressure,
                "x-ddb-loss-signaling": stream.loss_signaling,
            }),
        );
        let mut operation = json!({
            "action": "send",
            "channel": {"$ref": format!("#/channels/{}", stream.channel)},
            "messages": [{
                "$ref": format!(
                    "#/channels/{}/messages/{message_name}",
                    stream.channel
                )
            }],
            "bindings": {
                "http": {
                    "method": registry.http.method.to_uppercase(),
                    "bindingVersion": "0.3.0"
                }
            },
            "x-ddb-required-scope": operation_spec.permission.as_str(),
        });
        if operation_spec.permission.as_str() == "public" {
            operation["security"] = json!([]);
        }
        operations.insert(format!("send{}", operation_spec.method), operation);
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
            "description": "Replayable state and independent debugger output streams generated from the canonical DDB operation registry."
        },
        "defaultContentType": registry.http.unary_response_content_type,
        "servers": {
            "local": {
                "host": "127.0.0.1:8080",
                "protocol": "http",
                "description": "Default local endpoint; use GetCapabilities for the effective endpoint."
            }
        },
        "channels": Value::Object(channels),
        "operations": Value::Object(operations),
        "x-ddb-operation-registry": "./operation-registry-v2.json",
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
    registry: &OperationRegistry,
    openapi: &Value,
    asyncapi: &Value,
) -> Result<()> {
    if openapi["x-ddb-max-request-bytes"] != json!(registry.http.max_request_bytes) {
        bail!("OpenAPI request limit disagrees with the operation registry");
    }

    let paths = openapi["paths"]
        .as_object()
        .context("OpenAPI paths must be an object")?;
    let expected_paths = registry
        .operations
        .iter()
        .map(|operation| operation.path.as_str())
        .collect::<BTreeSet<_>>();
    let actual_paths = paths.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if expected_paths != actual_paths {
        bail!(
            "OpenAPI paths disagree with the operation registry: registry={expected_paths:?}, openapi={actual_paths:?}"
        );
    }

    let error_statuses = registry
        .errors
        .iter()
        .map(|error| error.status.to_string())
        .collect::<BTreeSet<_>>();
    for operation in &registry.operations {
        let path_item = paths[&operation.path]
            .as_object()
            .context("OpenAPI path item must be an object")?;
        if path_item.len() != 1 {
            bail!(
                "OpenAPI path {} has unexpected HTTP methods",
                operation.path
            );
        }
        let contract = path_item.get(&registry.http.method).with_context(|| {
            format!(
                "OpenAPI omitted {} {}",
                registry.http.method, operation.path
            )
        })?;
        if contract["operationId"] != operation.operation_id
            || contract["x-ddb-registry-key"] != operation.key
            || contract["x-ddb-protobuf-method"] != operation.protobuf_method
        {
            bail!("OpenAPI identity drift for {}", operation.key);
        }

        let expected_security = if operation.permission.as_str() == "public" {
            json!([])
        } else {
            json!([{"bearerAuth": []}])
        };
        let expected_scope = if operation.permission.as_str() == "public" {
            Value::Null
        } else {
            Value::String(operation.permission.as_str().to_string())
        };
        if contract["security"] != expected_security
            || contract
                .get("x-ddb-required-scope")
                .cloned()
                .unwrap_or(Value::Null)
                != expected_scope
        {
            bail!("OpenAPI permission drift for {}", operation.key);
        }

        let request_content = contract["requestBody"]["content"]
            .as_object()
            .context("OpenAPI request content must be an object")?;
        if request_content.len() != 1
            || request_content[&registry.http.request_content_type]["schema"]
                != schema_reference(&operation.input_type)
        {
            bail!("OpenAPI request schema/content drift for {}", operation.key);
        }

        let responses = contract["responses"]
            .as_object()
            .context("OpenAPI responses must be an object")?;
        let actual_errors = responses
            .keys()
            .filter(|status| status.as_str() != registry.http.success_status.to_string())
            .cloned()
            .collect::<BTreeSet<_>>();
        if error_statuses != actual_errors {
            bail!("OpenAPI error-status coverage drift for {}", operation.key);
        }
        for status in &error_statuses {
            if responses[status]["$ref"] != format!("#/components/responses/DdbError{status}") {
                bail!("OpenAPI error response drift for {}", operation.key);
            }
        }

        let success = &responses[&registry.http.success_status.to_string()];
        let success_content = success["content"]
            .as_object()
            .context("OpenAPI success content must be an object")?;
        let expected_content_type = if operation.server_streaming {
            &registry.http.stream_response_content_type
        } else {
            &registry.http.unary_response_content_type
        };
        if success_content.len() != 1 {
            bail!("OpenAPI success content drift for {}", operation.key);
        }
        let success_schema = &success_content[expected_content_type]["schema"];
        if operation.server_streaming {
            if success_schema["type"] != "string"
                || success_schema["format"] != "ndjson"
                || success_schema["x-ddb-stream-message"]
                    != schema_reference(&operation.output_type)
            {
                bail!("OpenAPI stream response drift for {}", operation.key);
            }
        } else if *success_schema != schema_reference(&operation.output_type) {
            bail!("OpenAPI response schema drift for {}", operation.key);
        }
    }

    let stream_operations = registry
        .operations
        .iter()
        .filter(|operation| operation.server_streaming)
        .collect::<Vec<_>>();
    let expected_channels = stream_operations
        .iter()
        .map(|operation| {
            operation
                .stream
                .as_ref()
                .expect("validated stream policy")
                .channel
                .as_str()
        })
        .collect::<BTreeSet<_>>();
    let channels = asyncapi["channels"]
        .as_object()
        .context("AsyncAPI channels must be an object")?;
    let actual_channels = channels.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if expected_channels != actual_channels {
        bail!(
            "AsyncAPI channels disagree with the stream registry: registry={expected_channels:?}, asyncapi={actual_channels:?}"
        );
    }

    let expected_operations = stream_operations
        .iter()
        .map(|operation| format!("send{}", operation.method))
        .collect::<BTreeSet<_>>();
    let operations = asyncapi["operations"]
        .as_object()
        .context("AsyncAPI operations must be an object")?;
    let actual_operations = operations.keys().cloned().collect::<BTreeSet<_>>();
    if expected_operations != actual_operations {
        bail!(
            "AsyncAPI operations disagree with the stream registry: registry={expected_operations:?}, asyncapi={actual_operations:?}"
        );
    }

    let messages = asyncapi["components"]["messages"]
        .as_object()
        .context("AsyncAPI message catalog must be an object")?;
    let expected_messages = stream_operations
        .iter()
        .map(|operation| {
            component_name(&operation.output_type).context("stream output must be an API component")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let actual_messages = messages.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if expected_messages != actual_messages {
        bail!(
            "AsyncAPI messages disagree with streaming response types: registry={expected_messages:?}, asyncapi={actual_messages:?}"
        );
    }

    for operation in stream_operations {
        let stream = operation
            .stream
            .as_ref()
            .context("validated streaming operation has no stream policy")?;
        let message_name = component_name(&operation.output_type)
            .context("stream output must be an API component")?;
        let channel = &channels[&stream.channel];
        let expected_channel = json!({
            "address": operation.path,
            "x-ddb-registry-key": operation.key,
            "x-ddb-protobuf-method": operation.protobuf_method,
            "x-ddb-request-schema": schema_reference(&operation.input_type),
            "x-ddb-lane": stream.lane,
            "x-ddb-heartbeat-seconds": stream.heartbeat_seconds,
            "x-ddb-cursor-replay": stream.cursor_replay,
            "x-ddb-ordering": stream.ordering,
            "x-ddb-replay-limits": stream.replay_limits,
            "x-ddb-backpressure": stream.backpressure,
            "x-ddb-loss-signaling": stream.loss_signaling,
        });
        for key in [
            "address",
            "x-ddb-registry-key",
            "x-ddb-protobuf-method",
            "x-ddb-request-schema",
            "x-ddb-lane",
            "x-ddb-heartbeat-seconds",
            "x-ddb-cursor-replay",
            "x-ddb-ordering",
            "x-ddb-replay-limits",
            "x-ddb-backpressure",
            "x-ddb-loss-signaling",
        ] {
            if channel[key] != expected_channel[key] {
                bail!("AsyncAPI channel {} {key} drift", stream.channel);
            }
        }
        if channel["messages"][message_name]["$ref"]
            != format!("#/components/messages/{message_name}")
        {
            bail!("AsyncAPI channel {} message drift", stream.channel);
        }

        let operation_name = format!("send{}", operation.method);
        let async_operation = &operations[&operation_name];
        if async_operation["action"] != "send"
            || async_operation["channel"]["$ref"] != format!("#/channels/{}", stream.channel)
            || async_operation["messages"][0]["$ref"]
                != format!("#/channels/{}/messages/{message_name}", stream.channel)
            || async_operation["bindings"]["http"]["method"] != registry.http.method.to_uppercase()
            || async_operation["x-ddb-required-scope"] != operation.permission.as_str()
        {
            bail!("AsyncAPI operation {operation_name} drift");
        }

        let message = &messages[message_name];
        if message["contentType"] != registry.http.unary_response_content_type
            || message["payload"] != schema_reference(&operation.output_type)
        {
            bail!("AsyncAPI message {message_name} payload drift");
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
