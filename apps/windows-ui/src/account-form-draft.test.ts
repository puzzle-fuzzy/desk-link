import { expect, test } from "bun:test";
import { AccountFormDraft } from "./account-form-draft";

test("validation and network failure preserve editable input, mode switches can clear secrets", () => {
  const draft = new AccountFormDraft();
  const data = new FormData();
  data.set("email", " person@example.com ");
  data.set("password", "synthetic-pass-12");
  data.set("confirmation", "mismatch");
  draft.capture(data);
  expect(draft.validate("register")?.field).toBe("confirmation");
  const fields = new Map(["email", "password", "confirmation"].map((name) => [name, { value: "" }]));
  draft.restore({ querySelector: (selector: string) => fields.get(selector.match(/name="(.*?)"/)![1]!) } as HTMLFormElement);
  expect(fields.get("email")!.value).toBe("person@example.com");
  expect(fields.get("password")!.value).toBe("synthetic-pass-12");
  draft.clearSecrets();
  expect(draft.email).toBe("person@example.com");
  expect(draft.password).toBe("");
});

test("successful registration pins resend to submitted email independently of editable DOM", () => {
  const draft = new AccountFormDraft();
  draft.password = "synthetic-pass-12";
  draft.registered("registered@example.com");
  draft.email = "edited@example.com";
  expect(draft.pendingVerificationEmail).toBe("registered@example.com");
  expect(draft.password).toBe("");
  draft.leave();
  expect(draft.email).toBe("");
  expect(draft.pendingVerificationEmail).toBe("");
});
