import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { AccountFormDraft } from "./account-form-draft";

const source = readFileSync(new URL("./main.ts", import.meta.url), "utf8");
const submit = source.slice(source.indexOf("async function submitAccount("), source.indexOf("\nasync function logoutAccount("));
const resend = source.slice(source.indexOf("async function resendVerification("), source.indexOf("\nasync function submitAccount("));

function host(mode: string, register: () => Promise<void>) {
  const accountDraft = new AccountFormDraft();
  const data = new FormData();
  data.set("email", "person@example.com");
  data.set("password", "synthetic-pass-12");
  data.set("confirmation", "synthetic-pass-12");
  const resendTargets: string[] = [];
  const fn = new Function("accountDraft", "data", "accountRegister", "accountResendVerification", "initialMode", `
    let accountMode = initialMode, accountBusy = false, accountError = null, accountErrorField = null,
      accountNotice = null, accountCanResendVerification = false;
    const FormData = function() { return data; };
    const render = () => {}, normalizeError = error => error.message;
    ${new Bun.Transpiler({loader: "ts"}).transformSync(submit + resend)}
    return { submit: () => submitAccount({}), resend: resendVerification,
      state: () => ({accountError, accountErrorField, accountMode, accountCanResendVerification}) };
  `);
  return { accountDraft, data, resendTargets, ...fn(accountDraft, data, register, async (email: string) => {resendTargets.push(email);}, mode) };
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
