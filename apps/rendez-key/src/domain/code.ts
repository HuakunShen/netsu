export const CODE_ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
export const CODE_LENGTH = 8;

export function generateCode(): string {
  const randomBytes = crypto.getRandomValues(new Uint8Array(5));
  let randomValue = 0n;

  for (const byte of randomBytes) {
    randomValue = (randomValue << 8n) | BigInt(byte);
  }

  let output = "";

  for (let shift = 35n; shift >= 0n; shift -= 5n) {
    const alphabetIndex = Number((randomValue >> shift) & 31n);
    output += CODE_ALPHABET[alphabetIndex];
  }

  if (output.length !== CODE_LENGTH) {
    throw new Error("invalid_code_generation_state");
  }

  return output;
}

export function formatCode(code: string): string {
  return `${code.slice(0, 4)}-${code.slice(4)}`;
}

export function normalizeCode(input: string): string | null {
  const normalized = input.toUpperCase().replace(/[\s-]+/g, "");

  if (normalized.length !== CODE_LENGTH) {
    return null;
  }

  for (const character of normalized) {
    if (!CODE_ALPHABET.includes(character)) {
      return null;
    }
  }

  return normalized;
}
