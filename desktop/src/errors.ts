export interface ApiError {
  code: string;
  message: string;
}
export function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  return "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}
export function errorText(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    const code = errorCode(error);
    return code ? `${code}: ${error.message}` : error.message;
  }
  return String(error);
}
