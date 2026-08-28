/** Unicolor: drop the colour parameters from SGR sequences (CSI … m) on the
    way into xterm.js, keeping every other attribute — bold, dim, italic,
    underline, blink, inverse, hidden, strike-through, and reset — so a harness
    renders in the theme's text colour without losing its emphasis.

    Output arrives in arbitrary chunks, so an escape sequence can be split
    across two writes; the filter carries an unfinished sequence to the next
    call and never emits a torn one. */

const SGR_SEQUENCE = /\x1b\[([\d;:]*)m/g;
/** An ESC, or ESC [ followed by SGR parameter bytes, with no final byte yet. */
const PARTIAL_SGR = /\x1b(?:\[[\d;:]*)?$/;
const MAX_CARRY = 64;

function isColourParam(value: number): boolean {
  return (
    (value >= 30 && value <= 37)
    || value === 39
    || (value >= 40 && value <= 47)
    || value === 49
    || (value >= 90 && value <= 97)
    || (value >= 100 && value <= 107)
  );
}

/** The SGR parameter list without its colour parameters, or `null` when the
    sequence carried nothing but colour and must be dropped whole (an empty
    `ESC[m` would mean "reset", which the harness never asked for). */
export function stripSgrColours(params: string): string | null {
  if (params === "") {
    return "";
  }
  const tokens = params.split(";");
  const kept: string[] = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    // Colon sub-parameter form: 38:2::r:g:b / 38:5:n / 58:… in one token.
    if (/^(38|48|58):/.test(token)) {
      continue;
    }
    const value = token === "" ? 0 : Number(token);
    if (!Number.isInteger(value)) {
      kept.push(token);
      continue;
    }
    if (value === 38 || value === 48 || value === 58) {
      // Extended colour: 38;5;n or 38;2;r;g;b — consume the argument tokens.
      const mode = Number(tokens[index + 1]);
      if (mode === 5) {
        index += 2;
      } else if (mode === 2) {
        index += 4;
      } else {
        index += 1;
      }
      continue;
    }
    if (isColourParam(value)) {
      continue;
    }
    kept.push(token);
  }
  if (kept.length === 0) {
    return null;
  }
  return kept.join(";");
}

export class SgrColourFilter {
  private carry = "";

  /** Filter one output chunk. Returns what may be written now. */
  apply(chunk: string): string {
    let text = this.carry + chunk;
    this.carry = "";
    const partial = PARTIAL_SGR.exec(text);
    if (partial && partial[0].length <= MAX_CARRY && partial[0].length < text.length + 1) {
      this.carry = partial[0];
      text = text.slice(0, partial.index);
    }
    return text.replace(SGR_SEQUENCE, (whole, params: string) => {
      const kept = stripSgrColours(params);
      if (kept === null) {
        return "";
      }
      return kept === params ? whole : `\x1b[${kept}m`;
    });
  }

  /** Hand back any carried partial sequence untouched (used when the filter
      is switched off mid-stream). */
  flush(): string {
    const carried = this.carry;
    this.carry = "";
    return carried;
  }
}
