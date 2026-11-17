const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

// Use the shipped parser, not a test-only copy. No DOM or packages are needed.
const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
const source = html.match(/<script id="failure-parser">([\s\S]*?)<\/script>/)[1];
const context = vm.createContext({ TextEncoder });
vm.runInContext(`${source}\nglobalThis.parser = FailureFocus;`, context);
const parse = text => JSON.parse(JSON.stringify(context.parser.parse(text)));

test('separates identical test names across suites and keeps exact context line numbers', () => {
  const log = [
    '     Running unittests src/lib.rs (target/debug/deps/one)',
    'test tests::resize ... FAILED',
    'failures:',
    '---- tests::resize stdout ----',
    "thread 'tests::resize' (123) panicked at src/first.rs:4:5:",
    'assertion `left == right` failed',
    'failures:',
    '    tests::resize',
    'test result: FAILED. 3 passed; 1 failed; 2 ignored; 0 measured;',
    '     Running unittests src/lib.rs (target/debug/deps/two)',
    'test tests::resize ... FAILED',
    '---- tests::resize stdout ----',
    "thread 'tests::resize' panicked at src/second.rs:8:9:",
    'different failure',
    'test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured;',
  ].join('\n');
  const result = parse(log);
  assert.equal(result.failures.length, 2);
  assert.equal(result.passed, 7);
  assert.equal(result.failed, 2);
  assert.equal(result.ignored, 2);
  assert.equal(result.summaries, 2);
  assert.equal(result.failures[0].location, 'src/first.rs:4:5');
  assert.equal(result.failures[1].location, 'src/second.rs:8:9');
  assert.deepEqual(result.failures[0].ranges, [{ start: 3, end: 6 }]);
  assert.deepEqual(result.failures[1].ranges, [{ start: 11, end: 14 }]);
  assert.equal(result.lines[13], 'different failure');
});

test('handles ANSI color and OSC titles without losing CRLF line positions', () => {
  const result = parse('\x1b]0;test run\x07\x1b[31mtest colored ... FAILED\x1b[0m\r\n---- colored stdout ----\r\nmessage\r\n');
  assert.equal(result.failures[0].name, 'colored');
  assert.equal(result.lines[0], 'test colored ... FAILED');
  assert.equal(result.lines[2], 'message');
  assert.deepEqual(result.failures[0].ranges, [{ start: 1, end: 4 }]);
});

test('keeps a partial status-only failure and does not invent a completed summary', () => {
  const result = parse('running 2 tests\ntest partial ... FAILED\n');
  assert.equal(result.failures.length, 1);
  assert.equal(result.failures[0].statusLine, 1);
  assert.deepEqual(result.failures[0].ranges, []);
  assert.equal(result.failures[0].location, null);
  assert.equal(result.summaries, 0);
});

test('recognizes details without status lines and combines stdout and stderr', () => {
  const result = parse('---- partial stdout ----\noutput\n---- partial stderr ----\nerror\nfailures:\n    partial');
  assert.equal(result.failures.length, 1);
  assert.deepEqual(result.failures[0].ranges, [{ start: 0, end: 2 }, { start: 2, end: 4 }]);
});

test('all-passing and unrecognized logs remain distinct from a failure verdict', () => {
  const passing = parse('test fine ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;');
  assert.equal(passing.failures.length, 0);
  assert.equal(passing.summaries, 1);
  assert.equal(passing.passed, 1);
  const unknown = parse('error[E0308]: mismatched types\nFAILED to compile');
  assert.equal(unknown.failures.length, 0);
  assert.equal(unknown.summaries, 0);
  assert.equal(unknown.lines[0], 'error[E0308]: mismatched types');
});

test('retains HTML-like log text literally for text-only rendering', () => {
  const text = 'test unsafe ... FAILED\n---- unsafe stdout ----\n<img src=x onerror=alert(1)> & <script>not executable';
  assert.equal(parse(text).lines[2], '<img src=x onerror=alert(1)> & <script>not executable');
});

test('bounds input bytes (including UTF-8) and line count instead of silently truncating', () => {
  assert.throws(() => parse('a'.repeat(1024 * 1024 + 1)), /larger than 1 MiB/);
  assert.throws(() => parse('界'.repeat(400000)), /larger than 1 MiB/);
  assert.throws(() => parse('\n'.repeat(20000)), /more than 20,000 lines/);
  assert.equal(parse('a'.repeat(1024 * 1024)).lines[0].length, 1024 * 1024);
  assert.equal(parse('\n'.repeat(19999)).lines.length, 20000);
});

test('attaches expected-panic diagnostics to their suffixed status record', () => {
  const result = parse('test tests::rejects_input - should panic ... FAILED\nfailures:\n---- tests::rejects_input stdout ----\nnote: test did not panic as expected\nfailures:\n    tests::rejects_input');
  assert.equal(result.failures.length, 1);
  assert.equal(result.failures[0].name, 'tests::rejects_input');
  assert.deepEqual(result.failures[0].ranges, [{ start: 2, end: 4 }]);
});

test('keeps captured Running text inside diagnostics rather than a new suite', () => {
  const result = parse('test migration ... FAILED\n---- migration stdout ----\nRunning migrations\nRunning unittests pretend.rs (fake)\nthread \'migration\' panicked at src/db.rs:9:2:\nmissing schema\nfailures:\n    migration');
  assert.equal(result.failures[0].location, 'src/db.rs:9:2');
  assert.deepEqual(result.failures[0].ranges, [{ start: 1, end: 6 }]);
});
