import test from 'node:test'
import assert from 'node:assert/strict'
import { slugify } from '../src/slug.mjs'

test('lowercases and joins words with dashes', () => {
  assert.equal(slugify('Hello World'), 'hello-world')
})
test('drops punctuation', () => {
  assert.equal(slugify('Hello, World!'), 'hello-world')
})
test('collapses runs of separators', () => {
  assert.equal(slugify('a  --  b'), 'a-b')
})
test('trims leading and trailing dashes', () => {
  assert.equal(slugify('  --Hello--  '), 'hello')
})
test('strips accents', () => {
  assert.equal(slugify('Crème Brûlée'), 'creme-brulee')
})
