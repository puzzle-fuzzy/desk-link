import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { AccountFormDraft } from "./account-form-draft";
import { AccountOperations } from "./account-operation";

const source = readFileSync(new URL("./main.ts", import.meta.url), "utf8");
const submit = source.slice(source.indexOf("async function submitAccount("), source.indexOf("\nasync function logoutAccount("));
const resend = source.slice(source.indexOf("async function resendVerification("), source.indexOf("\nasync function submitAccount("));

function host(mode: string, register: () => Promise<void>, login: () => Promise<unknown> = async () => ({signedIn: true})) {
  const accountDraft = new AccountFormDraft();
  const data = new FormData();
  data.set("email", "person@example.com");
  data.set("password", "synthetic-pass-12");
  data.set("confirmation", "synthetic-pass-12");
  const resendTargets: string[] = [];
  const accountOperations = new AccountOperations(1);
  const accepted: number[] = [];
  const workspaces: number[] = [];
  const fn = new Function("accountDraft", "data", "accountRegister", "accountResendVerification", "initialMode", "accountOperations", "accountLogin", "accountAcceptOperation", "startWorkspace", `
    let accountMode = initialMode, accountBusy = false, accountError = null, accountErrorField = null,
      accountNotice = null, accountCanResendVerification = false;
    const FormData = function() { return data; };
    let account = {signedIn: false}, accountLocalMode = false;
    const accountFinishOperation = async () => {}, accountCancelOperation = async () => {}, setAccountLocalModeEnabled = () => {};
    const accountForgotPassword = async () => {};
    const render = () => {}, normalizeError = error => error.message;
    ${new Bun.Transpiler({loader: "ts"}).transformSync(submit + resend)}
    return { submit: () => submitAccount({}), resend: resendVerification,
      switchMode(mode) { invalidateAccountOperation(); accountMode = mode; },
      local() { invalidateAccountOperation(); accountLocalMode = true; },
      state: () => ({accountError, accountErrorField, accountMode, accountCanResendVerification, accountLocalMode, accountBusy, account}) };
  `);
  return { accountDraft, data, resendTargets, accepted, workspaces, ...fn(accountDraft, data, register, async (email: string) => {resendTargets.push(email);}, mode, accountOperations, login, async (generation: number) => {accepted.push(generation);}, async () => {workspaces.push(1);}) };
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
  flow.local();
  response.resolve({signedIn: true});
  await old;
  expect(flow.state().accountLocalMode).toBe(true);
  expect(flow.state().account.signedIn).toBe(false);
  expect(flow.accepted).toEqual([]);
  expect(flow.workspaces).toEqual([]);
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
