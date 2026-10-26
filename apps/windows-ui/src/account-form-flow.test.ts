import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { AccountFormDraft } from "./account-form-draft";
import { AccountOperations } from "./account-operation";

const source = readFileSync(new URL("./main.ts", import.meta.url), "utf8");
const submit = source.slice(source.indexOf("async function submitAccount("), source.indexOf("\nasync function logoutAccount("));
const resend = source.slice(source.indexOf("async function resendVerification("), source.indexOf("\nasync function submitAccount("));
const localMode = source.slice(source.indexOf("async function enterAccountLocalMode("), source.indexOf("\nasync function resendVerification("));
const selectMode = source.slice(source.indexOf("  const selectAccountMode = "), source.indexOf("  accountTabs.forEach("));

function host(
  mode: string,
  register: () => Promise<void>,
  login: () => Promise<unknown> = async () => ({signedIn: true}),
  cancel: () => Promise<void> = async () => {},
) {
  const accountDraft = new AccountFormDraft();
  const data = new FormData();
  data.set("email", "person@example.com");
  data.set("password", "synthetic-pass-12");
  data.set("confirmation", "synthetic-pass-12");
  const resendTargets: string[] = [];
  const accountOperations = new AccountOperations(1);
  const accepted: number[] = [];
  const workspaces: number[] = [];
  const restarts: number[] = [];
  const localPreferences: boolean[] = [];
  const fn = new Function("accountDraft", "data", "accountRegister", "accountResendVerification", "initialMode", "accountOperations", "accountLogin", "accountAcceptOperation", "startWorkspace", "accountCancelOperation", "restartHost", "setAccountLocalModeEnabled", `
    let accountMode = initialMode, accountBusy = false, accountError = null, accountErrorField = null,
      accountNotice = null, accountCanResendVerification = false;
    const FormData = function() { return data; };
    let account = {signedIn: false}, accountLocalMode = false, feedback = null;
    const accountFinishOperation = async () => {};
    const window = { requestAnimationFrame() {} };
    const accountForgotPassword = async () => {};
    const render = () => {}, normalizeError = error => error.message;
    ${new Bun.Transpiler({loader: "ts"}).transformSync(submit + resend + localMode + selectMode)}
    return { submit: () => submitAccount({}), resend: resendVerification,
      switchMode(mode) { selectAccountMode({ dataset: {accountMode: mode} }); },
      local: enterAccountLocalMode,
      state: () => ({accountError, accountErrorField, accountMode, accountCanResendVerification, accountLocalMode, accountBusy, account}) };
  `);
  return { accountDraft, data, resendTargets, accepted, workspaces, restarts, localPreferences, ...fn(accountDraft, data, register, async (email: string) => {resendTargets.push(email);}, mode, accountOperations, login, async (generation: number) => {accepted.push(generation);}, async () => {workspaces.push(1);}, cancel, async () => { restarts.push(1); }, (enabled: boolean) => { localPreferences.push(enabled); }) };
}

test("actual account submit retains input after mismatch and network failure", async () => {
  const flow = host("register", async () => { throw new Error("network failed"); });
  flow.data.set("confirmation", "different");
  await flow.submit();
  expect(flow.state().accountErrorField).toBe("confirmation");
  expect(flow.accountDraft.email).toBe("person@example.com");
  flow.data.set("confirmation", "synthetic-pass-12");
  await flow.submit();
  expect(flow.state().accountError).toBe("network failed");
  expect(flow.accountDraft.password).toBe("synthetic-pass-12");
});

test("actual register and resend use the registered address after the form is replaced", async () => {
  const flow = host("register", async () => {});
  await flow.submit();
  expect(flow.state().accountCanResendVerification).toBe(true);
  flow.accountDraft.email = "edited@example.com";
  await flow.resend();
  expect(flow.resendTargets).toEqual(["person@example.com"]);
  expect(flow.accountDraft.password).toBe("");
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return {promise, resolve};
}

test("late registration cannot replace a newer forgot-password view or clear its busy state", async () => {
  const response = deferred<void>();
  const flow = host("register", () => response.promise);
  const old = flow.submit();
  flow.switchMode("forgot");
  response.resolve();
  await old;
  expect(flow.state().accountMode).toBe("forgot");
  expect(flow.state().accountCanResendVerification).toBe(false);
});

test("late login cannot persist or start workspace after choosing local mode", async () => {
  const response = deferred<unknown>();
  const flow = host("login", async () => {}, () => response.promise);
  const old = flow.submit();
  const local = flow.local();
  response.resolve({signedIn: true});
  await old;
  await local;
  expect(flow.state().accountLocalMode).toBe(true);
  expect(flow.state().account.signedIn).toBe(false);
  expect(flow.accepted).toEqual([]);
  expect(flow.workspaces).toEqual([1]);
});

test("a superseded retry cannot overwrite a newer accepted login", async () => {
  const oldResponse = deferred<unknown>();
  let calls = 0;
  const flow = host("login", async () => {}, () => ++calls === 1 ? oldResponse.promise : Promise.resolve({signedIn: true, user: "new"}));
  const old = flow.submit();
  flow.switchMode("login");
  await flow.submit();
  oldResponse.resolve({signedIn: true, user: "old"});
  await old;
  expect(flow.state().account.user).toBe("new");
  expect(flow.accepted).toHaveLength(1);
  expect(flow.workspaces).toEqual([1]);
});


test("failed native cancellation leaves local mode unaccepted and retryable without restarting Host", async () => {
  let attempts = 0;
  const flow = host("login", async () => {}, undefined, async () => {
    if (++attempts === 1) throw new Error("synthetic storage rollback failed");
  });
  flow.accountDraft.capture(flow.data);
  await flow.local();
  expect(flow.state().accountLocalMode).toBe(false);
  expect(flow.state().accountBusy).toBe(false);
  expect(flow.state().accountError).toContain("未能切换到本机模式，请重试");
  expect(flow.state().accountError).toContain("synthetic storage rollback failed");
  expect(flow.accountDraft.email).toBe("person@example.com");
  expect(flow.accountDraft.password).toBe("");
  expect(flow.localPreferences).toEqual([]);
  expect(flow.restarts).toEqual([]);
  expect(flow.workspaces).toEqual([]);
  await flow.local();
  expect(flow.state().accountLocalMode).toBe(true);
  expect(flow.state().accountError).toBeNull();
  expect(flow.localPreferences).toEqual([true]);
  expect(flow.restarts).toEqual([1]);
  expect(flow.workspaces).toEqual([1]);
});

test("pending native cancellation keeps the account gate until it has really completed", async () => {
  const cancellation = deferred<void>();
  const flow = host("login", async () => {}, undefined, () => cancellation.promise);
  const pending = flow.local();
  expect(flow.state().accountLocalMode).toBe(false);
  expect(flow.state().accountBusy).toBe(true);
  expect(flow.localPreferences).toEqual([]);
  expect(flow.restarts).toEqual([]);
  cancellation.resolve();
  await pending;
  expect(flow.state().accountLocalMode).toBe(true);
  expect(flow.restarts).toEqual([1]);
  expect(flow.workspaces).toEqual([1]);
});

test("a late local cancellation error cannot overwrite a newer account view", async () => {
  let reject!: (error: Error) => void;
  const oldCancellation = new Promise<void>((_, no) => { reject = no; });
  let attempts = 0;
  const flow = host("login", async () => {}, undefined, () => ++attempts === 1 ? oldCancellation : Promise.resolve());
  const old = flow.local();
  flow.switchMode("register");
  reject(new Error("stale rollback failure"));
  await old;
  expect(flow.state().accountMode).toBe("register");
  expect(flow.state().accountError).toBeNull();
  expect(flow.state().accountLocalMode).toBe(false);
  expect(flow.restarts).toEqual([]);
  expect(flow.workspaces).toEqual([]);
});

test("tab cancellation failures are handled and visible only for the owning view", async () => {
  let reject!: (error: Error) => void;
  const cancellation = new Promise<void>((_, no) => { reject = no; });
  const flow = host("login", async () => {}, undefined, () => cancellation);
  flow.switchMode("forgot");
  reject(new Error("current rollback failure"));
  await Promise.resolve();
  expect(flow.state().accountMode).toBe("forgot");
  expect(flow.state().accountError).toContain("未能完成账号切换，请重试");
  expect(flow.state().accountError).toContain("current rollback failure");
  expect(flow.state().accountBusy).toBe(false);
});

test("a superseded tab cancellation rejection cannot replace accepted login state", async () => {
  let reject!: (error: Error) => void;
  const cancellation = new Promise<void>((_, no) => { reject = no; });
  let attempts = 0;
  const flow = host("login", async () => {}, undefined, () => ++attempts === 1 ? cancellation : Promise.resolve());
  flow.switchMode("forgot");
  flow.switchMode("login");
  await flow.submit();
  reject(new Error("obsolete tab failure"));
  await Promise.resolve();
  expect(flow.state().account.signedIn).toBe(true);
  expect(flow.state().accountError).toBeNull();
  expect(flow.workspaces).toEqual([1]);
});
