export type AccountEnvironment = "development" | "test" | "production";

export interface AccountConfig {
  environment: AccountEnvironment;
  databasePath: string;
  publicOrigin: string;
  corsOrigins: string[];
  accessTokenTtlSeconds: number;
  refreshTokenTtlSeconds: number;
  mailUrl?: string;
  mailToken?: string;
  mailFrom?: string;
}

export function loadAccountConfig(
  environment: Record<string, string | undefined> = process.env,
): AccountConfig {
  const mode = environment.NODE_ENV ?? "development";
  const accountEnvironment: AccountEnvironment = mode === "production"
    ? "production"
    : mode === "test"
      ? "test"
      : "development";
  const config: AccountConfig = {
    environment: accountEnvironment,
    databasePath: environment.DESKLINK_ACCOUNT_DATABASE ?? ":memory:",
    publicOrigin: environment.DESKLINK_ACCOUNT_ORIGIN ?? "http://localhost:3412",
    corsOrigins: (environment.DESKLINK_ACCOUNT_CORS_ORIGINS ?? "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean),
    accessTokenTtlSeconds: 15 * 60,
    refreshTokenTtlSeconds: 30 * 24 * 60 * 60,
    mailUrl: environment.DESKLINK_ACCOUNT_MAIL_URL,
    mailToken: environment.DESKLINK_ACCOUNT_MAIL_TOKEN,
    mailFrom: environment.DESKLINK_ACCOUNT_MAIL_FROM,
  };

  try {
    const origin = new URL(config.publicOrigin);
    if (accountEnvironment === "production" && origin.protocol !== "https:") {
      throw new Error("DESKLINK_ACCOUNT_ORIGIN must use HTTPS in production");
    }
  } catch (error) {
    if (error instanceof Error && error.message.includes("must use HTTPS")) throw error;
    throw new Error("DESKLINK_ACCOUNT_ORIGIN must be a valid URL");
  }

  if (accountEnvironment === "production") {
    if (config.databasePath === ":memory:") {
      throw new Error("DESKLINK_ACCOUNT_DATABASE is required in production");
    }
    if (!config.mailUrl || !config.mailToken || !config.mailFrom) {
      throw new Error(
        "DESKLINK_ACCOUNT_MAIL_URL, DESKLINK_ACCOUNT_MAIL_TOKEN and DESKLINK_ACCOUNT_MAIL_FROM are required in production",
      );
    }
    try {
      if (new URL(config.mailUrl).protocol !== "https:") {
        throw new Error("DESKLINK_ACCOUNT_MAIL_URL must use HTTPS in production");
      }
    } catch (error) {
      if (error instanceof Error && error.message.includes("must use HTTPS")) throw error;
      throw new Error("DESKLINK_ACCOUNT_MAIL_URL must be a valid HTTPS URL");
    }
  }
  return config;
}
