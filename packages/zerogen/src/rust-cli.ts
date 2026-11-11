/**
 * The `zero-rust` CLI's `codegen`, from zero-rust.json: the Rust row types,
 * schema, and query builder of targets/rust.ts. Loading the schema and
 * writing or checking the output are shared with zero-swift (cli.ts).
 */
import {existsSync, readFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {Cli, z} from 'incur';
import {
  check,
  loadSchema,
  mustExist,
  reporting,
  writeOutput,
  type SchemaConfig,
} from './cli.ts';
import {
  generate,
  options as rustOptions,
  overrides as rustOverrides,
  type Config as RustConfig,
} from './targets/rust.ts';

export type Config = SchemaConfig & RustConfig;

const configFile = 'zero-rust.json';

export async function codegenRustFile(config: Config, cwd: string) {
  const {schema, customTypes, source} = await loadSchema(
    config,
    cwd,
    configFile,
  );
  return generate(schema, customTypes, config, source);
}

/** The `zero-rust` CLI: `codegen` from zero-rust.json. */
export function createRustCli(version: string) {
  return Cli.create('zero-rust', {
    version,
    description:
      "Generates what the Zero Rust client needs from a Zero app's TypeScript schema.",
  }).command('codegen', {
    description: `Generate the Rust row types, schema, and query builder from the app's TypeScript schema. Options override ${configFile}, which is read when present. See rust/README.md in github.com/wevm/zero.`,
    options: z.object({
      'config': z
        .string()
        .optional()
        .describe(`Config file (default ${configFile}, when present)`),
      'schema-file': z
        .string()
        .optional()
        .describe('TS schema module (default schema.ts)'),
      'export': z
        .string()
        .optional()
        .describe(
          'Export of the module that holds the schema (default schema)',
        ),
      'out': z
        .string()
        .optional()
        .describe('Rust file to write (default src/schema.rs)'),
      ...rustOptions,
      check,
    }),
    examples: [
      {description: `Generate from ${configFile}`},
      {
        options: {'schema-file': 'shared/schema.ts', 'out': 'src/schema.rs'},
        description: 'Generate without a config file',
      },
    ],
    hint: 'In CI, zero-rust codegen --check fails when the output is stale.',
    run: c =>
      reporting(c.error, async () => {
        const cwd = process.cwd();
        const {
          'config': configOption,
          'schema-file': schemaFile,
          out,
        } = c.options;
        const configPath =
          configOption ??
          (existsSync(resolve(cwd, configFile)) ? configFile : undefined);
        if (configPath) {
          mustExist(
            resolve(cwd, configPath),
            'CONFIG_NOT_FOUND',
            `Config ${configPath} doesn't exist.`,
          );
        }
        const fileConfig = configPath
          ? (JSON.parse(
              readFileSync(resolve(cwd, configPath), 'utf8'),
            ) as Config)
          : {};
        const config: Config = {
          ...fileConfig,
          ...rustOverrides(c.options),
          ...(c.options.export !== undefined ? {export: c.options.export} : {}),
          ...(schemaFile ? {schema: schemaFile} : {}),
          ...(out ? {output: out} : {}),
        };
        return writeOutput(
          resolve(cwd, config.output ?? 'src/schema.rs'),
          await codegenRustFile(config, cwd),
          c.options.check ?? false,
          cwd,
        );
      }),
  });
}
