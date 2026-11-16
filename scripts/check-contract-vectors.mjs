// Shared raw JSON parser vectors. Checks v2 decoded object keys and nesting.
import { readFileSync } from 'node:fs'
import assert from 'node:assert/strict'

function validate(raw) {
  let i = 0
  const ws = () => { while (/\s/.test(raw[i] ?? '') && i < raw.length) i++ }
  function string() {
    const start = i++
    while (i < raw.length) {
      if (raw[i] === '\\') { i += 2; continue }
      if (raw[i++] === '"') return JSON.parse(raw.slice(start, i))
    }
    throw Error('unterminated string')
  }
  function value(depth) {
    ws()
    if (raw[i] === '"') { string(); return }
    if (raw[i] === '{' || raw[i] === '[') {
      if (++depth > 64) throw Error('depth')
      const object = raw[i++] === '{', close = object ? '}' : ']'
      const keys = new Set()
      ws()
      if (raw[i] === close) { i++; return }
      while (true) {
        ws()
        if (object) {
          if (raw[i] !== '"') throw Error('key')
          const key = string()
          if (keys.has(key)) throw Error('duplicate')
          keys.add(key); ws()
          if (raw[i++] !== ':') throw Error('colon')
        }
        value(depth); ws()
        if (raw[i] === close) { i++; return }
        if (raw[i++] !== ',') throw Error('comma')
      }
    }
    const start = i
    while (i < raw.length && !/[\s,}\]]/.test(raw[i])) i++
    JSON.parse(raw.slice(start, i))
  }
  value(0); ws()
  if (i !== raw.length) throw Error('trailing content')
}

const vectors = JSON.parse(readFileSync(new URL('../fixtures/contracts/raw-validation-vectors.json', import.meta.url)))
for (const vector of vectors) {
  const data = vector.depth ? '['.repeat(vector.depth - 1) + '0' + ']'.repeat(vector.depth - 1) : vector.suffix
  const raw = `{"schema_version":${vector.schema},"data":${data}}`
  let accepted = true
  try { JSON.parse(raw); if (vector.schema === 2) validate(raw) } catch { accepted = false }
  assert.equal(accepted, vector.accepted, vector.name)
}
console.log(`Verified ${vectors.length} shared raw JSON vectors`)
