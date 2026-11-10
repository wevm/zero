/**
 * Emits the Rust for a Zero schema, against `zero_client::typed`: a row
 * struct per table with a constant per column and relationship, the types
 * of its enumerations and typed JSON columns, the runtime schema, and a
 * `zql` module with a function per table that starts a query.
 *
 * Identifiers are ASCII: the names of the schema stay in strings, so a
 * column of any name works. A name that would clash once it is an
 * identifier, or with what the generated code needs, gets `_` appended.
 */
import {z} from 'incur';
import type {Schema} from '../../../zero-types/src/schema.ts';
import type {CustomTypes, FieldModel, TypeModel} from '../types.ts';

export type Config = {
  /** The Rust file to write. Default `src/schema.rs`. */
  readonly output?: string | undefined;
  /** Prepended to every generated type name. Default none. */
  readonly typePrefix?: string | undefined;
  readonly tables?:
    | Readonly<
        Record<
          string,
          {
            /** The row struct's name. Default the table name in PascalCase. */
            readonly type?: string | undefined;
            readonly columns?:
              | Readonly<
                  Record<string, {readonly type?: 'int' | 'double' | undefined}>
                >
              | undefined;
          }
        >
      >
    | undefined;
};

export const options = {
  'type-prefix': z
    .string()
    .optional()
    .describe('Prefix for every generated type name (default none)'),
};

export function overrides(input: {'type-prefix'?: string | undefined}): Config {
  return input['type-prefix'] !== undefined
    ? {typePrefix: input['type-prefix']}
    : {};
}

/** Words Rust reserves; all but `rawless` can be written `r#word`. */
const keywords = new Set(
  (
    'as break const continue crate else enum extern false fn for if impl in ' +
    'let loop match mod move mut pub ref return self Self static struct super ' +
    'trait true type unsafe use where while async await dyn abstract become ' +
    'box do final macro override priv typeof unsized virtual yield try gen'
  ).split(' '),
);

/** Keywords `r#` can't escape. */
const rawless = new Set(['self', 'Self', 'super', 'crate', '_']);

/**
 * Type names the generated code uses unqualified, from the prelude, and
 * the types of `zero_client::typed` a reader may import next to the
 * generated ones.
 */
const reservedTypeNames = new Set([
  'Option',
  'Some',
  'None',
  'Result',
  'Ok',
  'Err',
  'Vec',
  'Box',
  'String',
  'ToString',
  'ToOwned',
  'Clone',
  'Copy',
  'Send',
  'Sync',
  'Sized',
  'Unpin',
  'Drop',
  'Fn',
  'FnMut',
  'FnOnce',
  'Iterator',
  'IntoIterator',
  'Extend',
  'Default',
  'Debug',
  'PartialEq',
  'Eq',
  'PartialOrd',
  'Ord',
  'Hash',
  'AsRef',
  'AsMut',
  'Into',
  'From',
  'TryFrom',
  'TryInto',
  'FromIterator',
  'Column',
  'ColumnRef',
  'Condition',
  'Direction',
  'DistinctRow',
  'ExistsOptions',
  'GroupRow',
  'Grouped',
  'GroupedQuery',
  'JsonPath',
  'Object',
  'Query',
  'Relationship',
  'RelationshipAggregates',
  'RowExpression',
  'Schema',
  'Value',
  'ViewEntry',
  'ViewValue',
  'ZeroRow',
  'ZeroValue',
  'ZeroValueError',
]);

/** The associated items of a row struct besides its columns'. */
const rowConsts = ['TABLE'];
/** The fields of a row struct besides its columns and relationships. */
const aggregatesField = 'aggregates';
/** The associated items of an enumeration besides its variants. */
const enumMembers = ['ALL'];

const wordBoundary = /([a-z0-9])([A-Z])/g;
const nonWord = /[^A-Za-z0-9]+/;
const leadingDigit = /^[0-9]/;

/**
 * The ASCII words of a name: runs of letters and digits, split where a
 * lowercase letter or digit meets an uppercase one. A name with none, such
 * as `+` or `é`, is one word: `x` and its code points in hex.
 */
function words(name: string): string[] {
  const out = name
    .replace(wordBoundary, '$1 $2')
    .split(nonWord)
    .filter(w => w !== '');
  if (out.length) {
    return out;
  }
  return [
    'x' + Array.from(name, ch => ch.codePointAt(0)!.toString(16)).join('x'),
  ];
}

/** `_` before a leading digit. */
function head(id: string): string {
  return leadingDigit.test(id) ? '_' + id : id;
}

function snakeCase(name: string): string {
  return head(
    words(name)
      .map(w => w.toLowerCase())
      .join('_'),
  );
}

function screamingCase(name: string): string {
  return head(
    words(name)
      .map(w => w.toUpperCase())
      .join('_'),
  );
}

/** A word all in capitals is a Rust word (`HTTP` is `Http`). */
function capitalize(w: string): string {
  const rest = w.slice(1);
  return (
    w[0].toUpperCase() +
    (w.length > 1 && w === w.toUpperCase() ? rest.toLowerCase() : rest)
  );
}

export function pascalCase(name: string): string {
  return head(words(name).map(capitalize).join(''));
}

/** An identifier as code: `r#` before a keyword that takes it. */
function ref(id: string): string {
  return keywords.has(id) ? `r#${id}` : id;
}

/**
 * The names of one Rust scope. A name that is taken, or that is a keyword
 * `r#` can't escape, gets `_` appended until it is free.
 */
class Members {
  readonly #used: Set<string>;

  constructor(reserved: Iterable<string> = []) {
    this.#used = new Set(reserved);
  }

  #free(id: string): boolean {
    return !this.#used.has(id) && !rawless.has(id);
  }

  claim(id: string): string {
    while (!this.#free(id)) {
      id += '_';
    }
    this.#used.add(id);
    return id;
  }

  /**
   * Claims an identifier for each name. The names that are their
   * identifier claim first, so a name that works as it is keeps it.
   */
  claimAll(names: readonly {name: string; id: string}[]): string[] {
    const out: string[] = [];
    const keep = names.map(n => n.id === n.name && this.#free(n.id));
    for (const pass of [true, false]) {
      for (const [i, n] of names.entries()) {
        if (keep[i] === pass) {
          out[i] = this.claim(n.id);
        }
      }
    }
    return out;
  }
}

/** A Rust string literal. */
function str(s: string): string {
  let out = '"';
  for (const ch of s) {
    const c = ch.codePointAt(0)!;
    if (ch === '"' || ch === '\\') {
      out += '\\' + ch;
    } else if (ch === '\n') {
      out += '\\n';
    } else if (ch === '\r') {
      out += '\\r';
    } else if (ch === '\t') {
      out += '\\t';
    } else if (c < 0x20 || c === 0x7f || (c >= 0xd800 && c <= 0xdfff)) {
      // A lone surrogate can't be in a Rust string.
      out += `\\u{${(c >= 0xd800 && c <= 0xdfff ? 0xfffd : c).toString(16)}}`;
    } else {
      out += ch;
    }
  }
  return out + '"';
}

/** A raw string literal of `s`, with as many `#`s as it needs. */
function rawStr(s: string): string {
  let hashes = '#';
  while (s.includes('"' + hashes)) {
    hashes += '#';
  }
  return `r${hashes}"${s}"${hashes}`;
}

function typeTaken(name: string, used: ReadonlySet<string>): boolean {
  return used.has(name) || reservedTypeNames.has(name) || keywords.has(name);
}

type Named =
  | {kind: 'enum'; name: string; cases: string[]}
  | {kind: 'object'; name: string; fields: FieldModel[]};

class Emitter {
  readonly #named: Named[] = [];
  readonly #names: Set<string>;
  /** Declared types by alias and shape, so a type used twice is one type. */
  readonly #byAlias = new Map<string, string>();
  readonly #prefix: string;

  constructor(prefix: string, rowNames: Iterable<string>) {
    this.#prefix = prefix;
    this.#names = new Set(rowNames);
  }

  bare(name: string): string {
    return this.#prefix && name.startsWith(this.#prefix)
      ? name.slice(this.#prefix.length)
      : name;
  }

  get named(): readonly Named[] {
    return this.#named;
  }

  /** A free type name: the alias, else `path`, else that with a number. */
  #claim(preferred: string | undefined, path: string): string {
    const free = (name: string) => !typeTaken(name, this.#names);
    const candidates = [preferred, path]
      .filter((n): n is string => n !== undefined)
      .map(n => this.#prefix + pascalCase(n));
    for (const name of candidates) {
      if (free(name)) {
        this.#names.add(name);
        return name;
      }
    }
    const fallback = this.#prefix + pascalCase(path);
    for (let i = 2; ; i++) {
      if (free(fallback + i)) {
        this.#names.add(fallback + i);
        return fallback + i;
      }
    }
  }

  /** The Rust type of a model, naming its enums and objects after `path`. */
  rustType(model: TypeModel, path: string): string {
    switch (model.kind) {
      case 'string':
        return 'String';
      case 'number':
        return 'f64';
      case 'boolean':
        return 'bool';
      case 'json':
        return 'z::Value';
      case 'nullable':
        return option(this.rustType(model.wrapped, path));
      case 'array':
        return `Vec<${this.rustType(model.element, path)}>`;
      case 'enum':
      case 'object': {
        const key = model.alias && `${model.alias}:${JSON.stringify(model)}`;
        const existing = key && this.#byAlias.get(key);
        if (existing) {
          return existing;
        }
        const name = this.#claim(model.alias, path);
        if (key) {
          this.#byAlias.set(key, name);
        }
        this.#named.push(
          model.kind === 'enum'
            ? {kind: 'enum', name, cases: model.cases}
            : {kind: 'object', name, fields: model.fields},
        );
        return name;
      }
    }
  }
}

function option(type: string): string {
  return type.startsWith('Option<') ? type : `Option<${type}>`;
}

type Column = {
  /** The column's name in the schema. */
  name: string;
  /** The field, without `r#`. */
  field: string;
  /** The constant. */
  constant: string;
  rustType: string;
};

type Rel = {
  name: string;
  field: string;
  constant: string;
  dest: string;
  one: boolean;
};

export function generate(
  schema: Schema,
  customTypes: CustomTypes,
  config: Config,
  source: string,
): string {
  const tableNames = Object.keys(schema.tables);
  const prefix = config.typePrefix ?? '';
  const rowNames = new Map<string, string>();
  const used = new Set<string>();
  for (const t of tableNames) {
    // A taken name gets `Row`, and then a number from 2.
    const preferred = config.tables?.[t]?.type ?? prefix + pascalCase(t);
    let name = preferred;
    for (let i = 1; typeTaken(name, used); i++) {
      name = `${preferred}Row${i === 1 ? '' : i}`;
    }
    used.add(name);
    rowNames.set(t, name);
  }
  const e = new Emitter(prefix, used);

  const out: string[] = [
    `// @generated by \`zero-rust codegen\` from ${source}. Do not edit.`,
    '//',
    '// Declare it as `#[rustfmt::skip] mod schema;`: rustfmt would reformat it,',
    '// and `zero-rust codegen --check` compares it as generated.',
    '#![allow(dead_code)]',
    '',
    'use ::zero_client::typed as z;',
    '',
  ];

  const rowDecls: string[] = [];
  for (const t of tableNames) {
    const table = schema.tables[t];
    const row = rowNames.get(t)!;
    const columns: Column[] = Object.entries(table.columns).map(([name, c]) => {
      const custom = customTypes.get(t)?.get(name);
      const override = config.tables?.[t]?.columns?.[name]?.type;
      const path = e.bare(row) + pascalCase(name);
      let base: string;
      if (override === 'int') {
        base = 'i64';
      } else if (c.type === 'number' || override === 'double') {
        base = 'f64';
      } else if (c.type === 'boolean') {
        base = 'bool';
      } else if (c.type === 'string') {
        base = custom?.kind === 'enum' ? e.rustType(custom, path) : 'String';
      } else if (c.type === 'json') {
        base = custom ? e.rustType(custom, path) : 'z::Value';
      } else {
        base = 'z::Value';
      }
      const rustType = c.optional === true ? option(base) : base;
      return {
        name,
        field: snakeCase(name),
        constant: screamingCase(name),
        rustType,
      };
    });
    const relationships: Rel[] = Object.entries(
      schema.relationships[t] ?? {},
    ).map(([name, hops]) => {
      const last = hops.at(-1)!;
      return {
        name,
        field: snakeCase(name),
        constant: screamingCase(name),
        dest: rowNames.get(last.destSchema)!,
        one: last.cardinality === 'one',
      };
    });
    const props = [...columns, ...relationships];
    const fields = new Members(
      relationships.length ? [aggregatesField] : [],
    ).claimAll(props.map(p => ({name: p.name, id: p.field})));
    const constants = new Members(rowConsts).claimAll(
      props.map(p => ({name: p.name, id: p.constant})),
    );
    for (const [i, p] of props.entries()) {
      p.field = fields[i];
      p.constant = constants[i];
    }

    const lines: string[] = [];
    const serverName = table.serverName
      ? ` (\`${table.serverName}\` on the server)`
      : '';
    lines.push(`/// A row of \`${t}\`${serverName}.`);
    lines.push('#[derive(Clone, Debug, PartialEq)]');
    lines.push(`pub struct ${row} {`);
    for (const c of columns) {
      lines.push(`    pub ${ref(c.field)}: ${c.rustType},`);
    }
    if (relationships.length) {
      lines.push(
        '    // Relationships, filled when a query asks for them with `related`.',
      );
      for (const r of relationships) {
        lines.push(
          `    pub ${ref(r.field)}: ${r.one ? `Option<Box<${r.dest}>>` : `Option<Vec<${r.dest}>>`},`,
        );
      }
      lines.push(
        '    /// The aggregates a query asked for in `related`.',
        `    pub ${aggregatesField}: z::RelationshipAggregates<${row}>,`,
      );
    }
    lines.push('}');
    lines.push('');
    lines.push(`impl ${row} {`);
    for (const c of columns) {
      lines.push(
        `    pub const ${c.constant}: z::Column<${row}, ${c.rustType}> = z::Column::new(${str(c.name)});`,
      );
    }
    for (const r of relationships) {
      lines.push(
        `    pub const ${r.constant}: z::Relationship<${row}, ${r.dest}> = z::Relationship::new(${str(r.name)});`,
      );
    }
    lines.push('}');
    lines.push('');
    lines.push(`impl z::QueryElement for ${row} {`);
    lines.push(
      '    fn from_entry(entry: &z::ViewEntry) -> Result<Self, z::ZeroValueError> {',
    );
    lines.push(`        Ok(${row} {`);
    // The index is the column's position in the table, where the rows have
    // it: reading it there skips looking the name up.
    for (const [i, c] of columns.entries()) {
      lines.push(
        `            ${ref(c.field)}: z::column(entry, ${str(c.name)}, ${i})?,`,
      );
    }
    for (const r of relationships) {
      lines.push(
        `            ${ref(r.field)}: z::${r.one ? 'one' : 'many'}(entry, ${str(r.name)})?,`,
      );
    }
    if (relationships.length) {
      lines.push(
        `            ${aggregatesField}: z::RelationshipAggregates::from_entry(entry),`,
      );
    }
    lines.push('        })');
    lines.push('    }');
    lines.push('}');
    lines.push('');
    lines.push(`impl z::ZeroRow for ${row} {`);
    lines.push(`    const TABLE: &'static str = ${str(t)};`);
    lines.push('');
    lines.push('    fn column_values(&self) -> z::Object {');
    lines.push(
      `        let mut row = z::Object::with_capacity(${columns.length});`,
    );
    for (const c of columns) {
      lines.push(
        `        row.insert(${str(c.name)}, z::ZeroValue::to_zero_value(&self.${ref(c.field)}));`,
      );
    }
    lines.push('        row');
    lines.push('    }');
    lines.push('}');
    lines.push('');
    lines.push(...optionalElement(row, 'optional_row'));
    rowDecls.push(lines.join('\n'));
  }

  // Named types are discovered while emitting the rows, and object fields
  // can name more of them.
  const namedDecls: string[] = [];
  for (let i = 0; i < e.named.length; i++) {
    const n = e.named[i];
    namedDecls.push(n.kind === 'enum' ? enumDecl(n) : objectDecl(e, n));
  }

  out.push(...rowDecls.flatMap(d => [d, '']));
  out.push(...namedDecls.flatMap(d => [d, '']));
  out.push(...schemaDecl(schema));
  out.push('');
  out.push(
    `/// The queries of the schema, one per table: \`zql::${tableNames[0] ? ref(snakeCase(tableNames[0])) : 'table'}()\`.`,
  );
  out.push('pub mod zql {');
  const fns = new Members().claimAll(
    tableNames.map(t => ({name: t, id: snakeCase(t)})),
  );
  for (const [i, t] of tableNames.entries()) {
    const row = rowNames.get(t)!;
    if (i > 0) out.push('');
    out.push(`    /// The rows of \`${t}\`.`);
    out.push(`    pub fn ${ref(fns[i])}() -> super::z::Query<super::${row}> {`);
    out.push('        super::z::Query::table(super::schema())');
    out.push('    }');
  }
  out.push('}');
  out.push('');
  return out.join('\n');
}

function optionalElement(type: string, read: string): string[] {
  return [
    `impl z::OptionalQueryElement for ${type} {`,
    '    fn optional_from_view(view: &z::ViewValue) -> Result<Option<Self>, z::ZeroValueError> {',
    `        z::${read}(view)`,
    '    }',
    '}',
  ];
}

function enumDecl(n: {name: string; cases: string[]}): string {
  const variants = new Members(enumMembers).claimAll(
    n.cases.map(value => ({name: value, id: pascalCase(value)})),
  );
  const lines = [
    `/// The cases of \`${n.name}\`.`,
    '#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]',
    `pub enum ${n.name} {`,
  ];
  for (const [i, value] of n.cases.entries()) {
    if (variants[i] !== value) lines.push(`    /// ${str(value)}`);
    lines.push(`    ${ref(variants[i])},`);
  }
  lines.push('}');
  lines.push('');
  lines.push(`impl ${n.name} {`);
  lines.push('    /// Every case, in declaration order.');
  lines.push(
    `    pub const ALL: [${n.name}; ${n.cases.length}] = [${variants.map(v => `${n.name}::${ref(v)}`).join(', ')}];`,
  );
  lines.push('');
  lines.push('    /// The string a row holds for this case.');
  lines.push("    pub fn as_str(self) -> &'static str {");
  lines.push('        match self {');
  for (const [i, value] of n.cases.entries()) {
    lines.push(`            ${n.name}::${ref(variants[i])} => ${str(value)},`);
  }
  lines.push('        }');
  lines.push('    }');
  lines.push('}');
  lines.push('');
  lines.push(`impl z::ZeroValue for ${n.name} {`);
  lines.push(
    '    fn from_zero_value(value: &z::Value) -> Result<Self, z::ZeroValueError> {',
  );
  lines.push('        match value.as_str() {');
  for (const [i, value] of n.cases.entries()) {
    lines.push(
      `            Some(${str(value)}) => Ok(${n.name}::${ref(variants[i])}),`,
    );
  }
  lines.push(
    `            _ => Err(z::ZeroValueError::mismatch(${str(`one of the cases of ${n.name}`)}, value)),`,
  );
  lines.push('        }');
  lines.push('    }');
  lines.push('');
  lines.push('    fn to_zero_value(&self) -> z::Value {');
  lines.push('        z::Value::String(self.as_str().to_owned())');
  lines.push('    }');
  lines.push('}');
  lines.push('');
  lines.push(`impl z::OrderedZeroValue for ${n.name} {`);
  lines.push(`    type Unwrapped = ${n.name};`);
  lines.push('}');
  lines.push('');
  lines.push(...optionalElement(n.name, 'optional_value'));
  return lines.join('\n');
}

function objectDecl(
  e: Emitter,
  n: {name: string; fields: FieldModel[]},
): string {
  const ids = new Members().claimAll(
    n.fields.map(f => ({name: f.name, id: snakeCase(f.name)})),
  );
  const fields = n.fields.map((f, i) => {
    const base = e.rustType(f.type, e.bare(n.name) + pascalCase(f.name));
    return {
      ...f,
      field: ref(ids[i]),
      rustType: f.optional ? option(base) : base,
    };
  });
  const lines = [
    `/// The \`${n.name}\` objects of a \`json\` column.`,
    '#[derive(Clone, Debug, PartialEq)]',
    `pub struct ${n.name} {`,
  ];
  for (const f of fields) {
    lines.push(`    pub ${f.field}: ${f.rustType},`);
  }
  lines.push('}');
  lines.push('');
  lines.push(`impl z::ZeroValue for ${n.name} {`);
  lines.push(
    '    fn from_zero_value(value: &z::Value) -> Result<Self, z::ZeroValueError> {',
  );
  lines.push(`        let object = z::object(value, ${str(n.name)})?;`);
  lines.push(`        Ok(${n.name} {`);
  for (const f of fields) {
    lines.push(`            ${f.field}: z::field(object, ${str(f.name)})?,`);
  }
  lines.push('        })');
  lines.push('    }');
  lines.push('');
  lines.push('    fn to_zero_value(&self) -> z::Value {');
  lines.push(
    `        let mut object = z::Object::with_capacity(${fields.length});`,
  );
  for (const f of fields) {
    // An optional field (`name?: T`) is left out when it is `None`.
    lines.push(
      f.optional
        ? `        if let Some(v) = &self.${f.field} {\n            object.insert(${str(f.name)}, z::ZeroValue::to_zero_value(v));\n        }`
        : `        object.insert(${str(f.name)}, z::ZeroValue::to_zero_value(&self.${f.field}));`,
    );
  }
  lines.push('        z::Value::Object(object)');
  lines.push('    }');
  lines.push('}');
  return lines.join('\n');
}

/** The schema in the JSON form `Schema::from_json` reads. */
function schemaDecl(schema: Schema): string[] {
  const tables: Record<string, unknown> = {};
  for (const [t, table] of Object.entries(schema.tables)) {
    const columns: Record<string, unknown> = {};
    for (const [c, col] of Object.entries(table.columns)) {
      columns[c] = {
        type: col.type,
        ...(col.optional ? {optional: true} : {}),
        ...(col.serverName ? {serverName: col.serverName} : {}),
      };
    }
    tables[t] = {
      name: t,
      ...(table.serverName ? {serverName: table.serverName} : {}),
      columns,
      primaryKey: table.primaryKey,
    };
  }
  const relationships: Record<string, unknown> = {};
  for (const [t, byName] of Object.entries(schema.relationships)) {
    if (Object.keys(byName).length === 0) continue;
    relationships[t] = Object.fromEntries(
      Object.entries(byName).map(([r, hops]) => [
        r,
        hops.map(h => ({
          sourceField: h.sourceField,
          destField: h.destField,
          destSchema: h.destSchema,
          cardinality: h.cardinality,
        })),
      ]),
    );
  }
  const json = JSON.stringify({tables, relationships}, null, 2);
  return [
    '/// The schema the queries of `zql` are over.',
    'pub fn schema() -> ::std::sync::Arc<z::Schema> {',
    '    static SCHEMA: ::std::sync::OnceLock<::std::sync::Arc<z::Schema>> = ::std::sync::OnceLock::new();',
    `    SCHEMA.get_or_init(|| z::schema_from_json(${rawStr(json)})).clone()`,
    '}',
  ];
}
