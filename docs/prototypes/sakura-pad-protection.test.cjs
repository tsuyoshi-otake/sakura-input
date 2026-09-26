// Pure interaction-state tests, not browser rendering or native Pad evidence.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { Script, createContext, runInContext } = require('node:vm');
const { test } = require('node:test');
const assert = require('node:assert/strict');

const html = readFileSync(join(__dirname, 'sakura-pad-protection.html'), 'utf8');
const source = html.match(/<script id="pad-demo-model">([\s\S]*?)<\/script>/)?.[1];
assert.ok(source, 'standalone prototype has the pure model block');
const sandbox = createContext({});
runInContext(source, sandbox, { timeout: 1000 });
const { Model } = runInContext('PadDemo', sandbox);
const password = 'Sakura-demo1';
const recovery = 'PAD-7K4M-92QF';
const memo = 'note-01';

function prepare(model, scope = 'whole', useRecovery = true) {
  const started = model.beginSetup(scope);
  if (started.reason === 'reauth-required') {
    assert.equal(model.authorizeSetup(scope, password).ok, true);
  } else {
    assert.equal(started.ok, true);
  }
  assert.equal(model.setPasswordDraft(password, password, 'password').ok, true);
  assert.equal(model.hideRecovery(), true);
  assert.equal(model.confirmRecovery('92QF', !useRecovery).ok, true);
}
function enable(model, scope = 'whole', useRecovery = true) {
  prepare(model, scope, useRecovery);
  const begun = model.commitSetup();
  assert.equal(begun.ok, true);
  assert.equal(model.finishSetup(begun.requestId, true).ok, true);
}
function unlock(model, scope) {
  const begun = model.beginUnlock(scope);
  assert.equal(begun.ok, true);
  assert.equal(model.finishUnlock(begun.requestId, password).ok, true);
}
function plain(value) { return JSON.parse(JSON.stringify(value)); }

test('all inline scripts parse without executing the DOM script', () => {
  const scripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 2);
  for (const [index, match] of scripts.entries()) {
    new Script(match[1], { filename: `prototype-inline-${index}.js` });
  }
  assert.doesNotMatch(source, /\b(?:window|document|localStorage|fetch|XMLHttpRequest)\s*[.(]/);
});

test('whole unlock exposes ordinary memo but does not unlock individual memo', () => {
  const model = new Model();
  enable(model);
  unlock(model, memo);
  assert.ok(model.visibleMemo(memo).body);
  model.lockWhole();
  assert.equal(model.visibleMemo(memo), null);
  assert.equal(model.visibleMemo('note-02'), null);
  unlock(model, 'whole');
  assert.ok(model.visibleMemo('note-02').body);
  assert.deepEqual(plain(model.visibleMemo(memo)), {
    id: memo, label: '保護されたメモ 01', locked: true,
  });
});

test('switching memo revokes individual unlock and later selection stays locked', () => {
  const model = new Model();
  unlock(model, memo);
  model.selectMemo('note-02');
  model.selectMemo(memo);
  assert.equal(model.visibleMemo(memo).locked, true);
  assert.equal(model.visibleMemo(memo).body, undefined);
});

test('cancelling note setup preserves committed whole protection and recovery', () => {
  const model = new Model();
  enable(model);
  const before = plain(model.snapshot().whole);
  unlock(model, memo);
  prepare(model, memo, false);
  model.cancelSetup();
  assert.deepEqual(plain(model.snapshot().whole), before);
  assert.equal(model.snapshot().draft, null);
});

test('changing enabled whole protection requires the existing password', () => {
  const model = new Model();
  enable(model);
  const before = plain(model.snapshot().whole);
  const started = model.beginSetup('whole');
  assert.equal(started.reason, 'reauth-required');
  assert.equal(model.snapshot().draft, null);
  assert.equal(model.snapshot().view, 'setupReauth');
  assert.equal(model.authorizeSetup('whole', 'wrong').reason, 'wrong-credential');
  assert.equal(model.snapshot().draft, null);
  assert.deepEqual(plain(model.snapshot().whole), before);
  assert.equal(model.authorizeSetup('whole', password).ok, true);
  assert.equal(model.snapshot().view, 'setupMethod');
  model.cancelSetup();
  assert.deepEqual(plain(model.snapshot().whole), before);
});

test('memo policy reauthentication is scoped and cancellation ends the attempt', () => {
  const model = new Model();
  unlock(model, memo);
  assert.equal(model.beginSetup(memo).reason, 'reauth-required');
  assert.equal(model.authorizeSetup('whole', password).reason, 'no-reauth-pending');
  assert.equal(model.cancelSetupReauth(), true);
  assert.equal(model.authorizeSetup(memo, password).reason, 'no-reauth-pending');
  assert.equal(model.snapshot().draft, null);
  assert.equal(model.snapshot().view, 'pad');
});

test('locked policy cannot start a protection-change draft', () => {
  const model = new Model();
  enable(model);
  model.lockWhole();
  assert.equal(model.beginSetup('whole').reason, 'protected');
  assert.equal(model.beginSetup(memo).reason, 'protected');
  assert.equal(model.snapshot().draft, null);
  unlock(model, 'whole');
  assert.equal(model.beginSetup(memo).reason, 'protected');
  assert.equal(model.lockMemo('note-02'), false);
  assert.equal(model.visibleMemo('note-02').locked, false);
});

test('parent Apply/Cancel owns only the shortcut draft', () => {
  const model = new Model();
  model.setShortcutDraft('ショートカットなし');
  enable(model);
  assert.equal(model.snapshot().shortcut.saved, 'Ctrl を2回押す');
  model.cancelParent();
  assert.equal(model.snapshot().shortcut.edited, 'Ctrl を2回押す');
  assert.equal(model.snapshot().whole.enabled, true);
  model.setShortcutDraft('ショートカットなし');
  model.applyParent();
  assert.equal(model.snapshot().shortcut.saved, 'ショートカットなし');
});

test('recovery challenge is unavailable until displayed key is masked', () => {
  const model = new Model();
  model.beginSetup('whole');
  assert.equal(model.revealRecovery(), recovery);
  assert.equal(model.confirmRecovery('92QF', false).ok, false);
  model.hideRecovery();
  assert.equal(model.revealRecovery(), null);
  assert.equal(model.confirmRecovery('wrong', false).ok, false);
  assert.equal(model.confirmRecovery('92QF', false).ok, true);
});

test('recovery only accepts a registered route and affects exactly its scope', () => {
  const model = new Model();
  enable(model, 'whole', false);
  model.lockWhole();
  assert.equal(model.recover('whole', recovery).ok, false);
  unlock(model, 'whole');
  assert.equal(model.recover(memo, 'wrong').ok, false);
  assert.equal(model.recover(memo, recovery).ok, true);
  assert.ok(model.visibleMemo(memo).body);
  enable(model, 'whole', true);
  model.lockWhole();
  assert.equal(model.recover('whole', recovery).ok, true);
  assert.equal(model.visibleMemo(memo).locked, true);
});

test('setup publication cannot cancel midway; failed save retains old policy', () => {
  const model = new Model();
  enable(model);
  const before = plain(model.snapshot().whole);
  prepare(model, 'whole', false);
  model.setTimeoutDraft('15分');
  const begun = model.commitSetup();
  assert.equal(model.cancelSetup(), false);
  assert.equal(model.commitSetup().ok, false);
  assert.equal(model.finishSetup(begun.requestId, false).ok, false);
  assert.deepEqual(plain(model.snapshot().whole), before);
  const retry = model.commitSetup();
  assert.equal(model.finishSetup(retry.requestId, true).ok, true);
  assert.equal(model.snapshot().whole.timeout, '15分');
  assert.equal(model.snapshot().whole.recovery, false);
});

test('duplicate and cancelled authentication cannot later unlock', () => {
  const model = new Model();
  const begun = model.beginUnlock(memo);
  assert.equal(model.beginUnlock(memo).ok, false);
  assert.equal(model.recover(memo, recovery).ok, false);
  assert.equal(model.cancelUnlock(begun.requestId), true);
  assert.equal(model.finishUnlock(begun.requestId, password).ok, false);
  assert.equal(model.visibleMemo(memo).locked, true);
});

test('wrong credential terminates the attempt without showing protected title', () => {
  const model = new Model();
  const begun = model.beginUnlock(memo);
  assert.equal(model.finishUnlock(begun.requestId, 'wrong').ok, false);
  assert.equal(model.snapshot().busy, null);
  assert.equal(model.visibleMemo(memo).title, undefined);
  assert.equal(model.beginUnlock(memo).ok, true);
});

test('relocking whole Pad invalidates a pending whole unlock', () => {
  const model = new Model();
  enable(model);
  model.lockWhole();
  const begun = model.beginUnlock('whole');
  model.lockWhole();
  assert.equal(model.finishUnlock(begun.requestId, password).ok, false);
  assert.equal(model.snapshot().whole.locked, true);
});

test('switching away invalidates pending memo authentication', () => {
  const model = new Model();
  const begun = model.beginUnlock(memo);
  model.selectMemo('note-02');
  assert.equal(model.finishUnlock(begun.requestId, password).ok, false);
  assert.equal(model.visibleMemo(memo).locked, true);
});

test('memo authentication and recovery are rejected while the whole Pad is locked', () => {
  const model = new Model();
  enable(model);
  model.lockWhole();
  assert.equal(model.beginUnlock(memo).ok, false);
  assert.equal(model.recover(memo, recovery).ok, false);
  assert.equal(model.visibleMemo(memo), null);
});

test('unconfigured whole Pad does not pretend to have a password lock', () => {
  const model = new Model();
  assert.equal(model.lockWhole(), false);
  assert.equal(model.snapshot().whole.locked, false);
});

test('memo relock invalidates a pending unlock for that memo', () => {
  const model = new Model();
  const begun = model.beginUnlock(memo);
  model.lockMemo(memo);
  assert.equal(model.finishUnlock(begun.requestId, password).ok, false);
  assert.equal(model.visibleMemo(memo).locked, true);
});

test('the in-flight protection publication uses its confirmed draft', () => {
  const model = new Model();
  prepare(model);
  model.setTimeoutDraft('10分');
  const begun = model.commitSetup();
  assert.equal(model.setTimeoutDraft('しない'), false);
  assert.equal(model.finishSetup(begun.requestId, true).ok, true);
  assert.equal(model.snapshot().whole.timeout, '10分');
});

test('lock while publishing completes publication without revealing the Pad', () => {
  const model = new Model();
  enable(model);
  prepare(model);
  const begun = model.commitSetup();
  model.lockWhole();
  assert.equal(model.visibleMemo('note-02'), null);
  assert.equal(model.finishSetup(begun.requestId, true).ok, true);
  assert.equal(model.snapshot().whole.locked, true);
  assert.equal(model.visibleMemo('note-02'), null);
});
