export type AccountMode = "login" | "register" | "forgot";

/** Lives only for the account gate. Never serialize this object. */
export class AccountFormDraft {
  email = "";
  password = "";
  confirmation = "";
  pendingVerificationEmail = "";

  capture(data: Pick<FormData, "get">): void {
    this.email = String(data.get("email") ?? "").trim();
    this.password = String(data.get("password") ?? "");
    this.confirmation = String(data.get("confirmation") ?? "");
  }

  validate(mode: AccountMode): { field: "email" | "password" | "confirmation"; message: string } | null {
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(this.email)) return { field: "email", message: "请填写有效邮箱。" };
    if (mode !== "forgot" && (this.password.length < 12 || this.password.length > 128)) return { field: "password", message: "请填写 12 至 128 个字符的密码。" };
    if (mode === "register" && this.password !== this.confirmation) return { field: "confirmation", message: "两次输入的密码不一致。" };
    return null;
  }

  restore(form: HTMLFormElement): void {
    for (const name of ["email", "password", "confirmation"] as const) {
      const input = form.querySelector<HTMLInputElement>(`input[name="${name}"]`);
      if (input) input.value = this[name];
    }
  }

  registered(email: string): void {
    this.pendingVerificationEmail = email;
    this.email = email;
    this.clearSecrets();
  }

  clearSecrets(): void {
    this.password = "";
    this.confirmation = "";
  }

  leave(): void {
    this.clearSecrets();
    this.pendingVerificationEmail = "";
    this.email = "";
  }
}
