import type { MailMessage, Mailer } from "./mailer";

/** Test/development adapter. It never sends an external email. */
export class DevMailer implements Mailer {
  readonly messages: MailMessage[] = [];

  async send(message: MailMessage): Promise<void> {
    this.messages.push(message);
  }
}
