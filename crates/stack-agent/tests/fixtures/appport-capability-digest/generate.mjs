// Regenerates vectors.json from AppPort's reference implementation, so Stack-Agent's digest is checked against
// AppPort's own code rather than against itself.
//
//   npm install --prefix /tmp/appport-protocol @appport/protocol@1.3.0
//   node generate.mjs /tmp/appport-protocol/node_modules/@appport/protocol/dist/index.js
//
// The payments-*.manifest.json files are manifests served by real @appport/server 1.1.5 hosts (the
// stable-references experiment in rkendel1/appport), with `transports` removed.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const entry = process.argv[2];
if (!entry) throw new Error('usage: node generate.mjs <path to @appport/protocol dist/index.js>');
const { canonicalize, digestOf, capabilityDigest } = await import(pathToFileURL(path.resolve(entry)).href);
const protocolPackage = JSON.parse(readFileSync(path.join(path.dirname(path.resolve(entry)), '..', 'package.json'), 'utf8'));
const here = path.dirname(fileURLToPath(import.meta.url));
const manifest = (name) => JSON.parse(readFileSync(path.join(here, name), 'utf8'));

// JSON text, parsed exactly as a consumer receives it. Each targets one rule of RFC 8785 / ECMAScript serialization.
const canonical = {
  'integers': '[0,-0,1,-1,100,9007199254740991,9007199254740993,12345678901234567890]',
  'fractions': '[0.1,1.5,-1.25,12.0,0.000001,1e-7,1e-6,5e-324,-1.25e-10]',
  'large': '[1e20,1e21,123456789012345680000,1.7976931348623157e308,1e300]',
  'fractions-precision': '[0.30000000000000004,1.0000000000000002,2.2250738585072011e-308,123.456789e-12,8.98846567431158e307,0.1e1,3.141592653589793238462643383279,9.007199254740993e15,1.7e-309]',
  'strings-escapes': '["\\u0000\\u001f\\b\\f\\n\\r\\t","\\"\\\\/","\\u007f","\\u2028\\u2029"]',
  'strings-non-ascii': '["é","中文","😀","\\ud83d\\ude00"]',
  'key-order-utf16': '{"b":1,"a":2,"aa":3,"A":4,"é":5,"😀":6,"\\uffff":7,"":8}',
  'nested': '{"z":[{"y":null,"x":true},[],{}],"a":{"c":false,"b":[1,[2,[3]]]}}',
  'whitespace': ' { "b" : [ 1 , 2 ] , "a" : "x" } ',
};

const integrated = {
  capabilities: [
    { name: 'tasks.create', versions: [1, 2], latestVersion: 2, definitions: [
      { version: 1, inputSchema: { type: 'object' }, outputSchema: { type: 'object' } },
      { version: 2, inputSchema: { type: 'object', properties: { title: { type: 'string', maxLength: 80 } } }, outputSchema: { type: 'object' }, timeoutMs: 1500.5 },
    ] },
  ],
  attributes: { 'appport.integration/1': { capabilities: { 'tasks.create@2': { purpose: 'Create a task', access: [{ resource: 'https://vocab.example.org/task', mode: 'changes' }] } } } },
};

const capabilities = [
  ['payments-a', manifest('payments-a.manifest.json'), 'payments.charge', 1],
  ['payments-c', manifest('payments-c.manifest.json'), 'payments.charge', 1],
  ['payments-a-absent-version', manifest('payments-a.manifest.json'), 'payments.charge', 2],
  ['payments-a-absent-name', manifest('payments-a.manifest.json'), 'payments.refund', 1],
  ['integration-entry', integrated, 'tasks.create', 2],
  ['integration-other-version', integrated, 'tasks.create', 1],
];

const vectors = {
  generatedBy: `@appport/protocol ${protocolPackage.version} (canonicalize, digestOf, capabilityDigest)`,
  canonical: Object.entries(canonical).map(([id, json]) => {
    const value = JSON.parse(json);
    return { id, json, canonical: canonicalize(value), digest: digestOf(value) };
  }),
  capabilities: capabilities.map(([id, m, name, version]) => ({
    id,
    manifest: m === integrated ? 'inline' : `${id.split('-').slice(0, 2).join('-')}.manifest.json`,
    ...(m === integrated ? { inline: integrated } : {}),
    name,
    version,
    digest: capabilityDigest(m, name, version) ?? null,
  })),
};
writeFileSync(path.join(here, 'vectors.json'), JSON.stringify(vectors, null, 2) + '\n');
console.log(`wrote ${vectors.canonical.length} canonical and ${vectors.capabilities.length} capability vectors`);
