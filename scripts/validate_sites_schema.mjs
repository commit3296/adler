import fs from "node:fs";

import Ajv2020 from "ajv/dist/2020.js";

const schemaPath = "docs/sites.schema.json";
const dataPath = "adler-core/data/sites.json";

const schema = JSON.parse(fs.readFileSync(schemaPath, "utf8"));
const data = JSON.parse(fs.readFileSync(dataPath, "utf8"));

const ajv = new Ajv2020({
  allErrors: true,
  strict: false,
});
const validate = ajv.compile(schema);

if (!validate(data)) {
  for (const error of validate.errors?.slice(0, 25) ?? []) {
    const loc = error.instancePath || "(root)";
    console.error(`::error::sites.json ${loc}: ${error.message}`);
  }
  process.exit(1);
}

console.log(`schema OK: ${data.sites.length} sites`);
