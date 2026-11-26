/*
 * Tool input as cosmonic:agent@0.4.2 describes it, carried through an agent
 * whose own tools take a JSON object.
 *
 * A tool takes a JSON object matching a schema, text a grammar constrains, or
 * plain text (`tool-input`: `json-schema`, `grammar` or `text`; the last two
 * are freeform tools), and the model's call carries `json` or `text`
 * arguments to match (`tool-arguments`). Pi holds every tool's input as a JSON
 * Schema and every call's arguments as an object, so two things ride with it:
 *
 *   - the `tool-input` each listed tool came with, by name, so a model request
 *     offers each tool exactly as the host listed it, freeform and grammar
 *     included (rememberListed, offeredInput);
 *   - a freeform call's text, as the one property FREEFORM_TEXT of the agent's
 *     arguments object, unwrapped verbatim whenever the call leaves the agent
 *     (agentArguments, toolArguments).
 *
 * No host imports, so it can be tested under Node.
 */

// The property a freeform call's text rides under in the agent's arguments.
// Named so that no JSON tool's arguments are mistaken for it.
export const FREEFORM_TEXT = 'x-cosmonic-freeform-text';

// Each listed tool's input, by name, from the last tools.list.
export type ToolInput =
    | { tag: 'text' }
    | { tag: 'json-schema'; val: string }
    | { tag: 'grammar'; val: { syntax: string; definition: string } };
type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };
type Arguments = { tag: 'text' | 'json'; val: string };

let listed = new Map<string, ToolInput>();

// Remember the inputs of the tools the host just listed ({name, input}),
// replacing the last list.
export function rememberListed(defs: { name: string; input: ToolInput }[]) {
    listed = new Map(defs.map((d) => [d.name, d.input]));
}

// Whether a tool with this input is a freeform tool: the model writes text,
// constrained by a grammar or not.
export function isFreeformInput(input?: ToolInput): input is Exclude<ToolInput, { tag: 'json-schema' }> {
    return input?.tag === 'grammar' || input?.tag === 'text';
}

// The JSON Schema the agent holds for a tool with this input: the listed
// schema, or for a freeform tool an object with the text as its one property.
export function agentSchema(input?: ToolInput): object {
    if (isFreeformInput(input)) {
        return { type: 'object', properties: { [FREEFORM_TEXT]: { type: 'string' } }, required: [FREEFORM_TEXT] };
    }
    try {
        return JSON.parse(input?.val || '{"type":"object"}');
    } catch {
        return { type: 'object' };
    }
}

// The input to offer the model for an agent tool ({name, parameters}): as the
// host listed it, or, for a tool the host did not list, its schema.
export function offeredInput(tool: { name: string; parameters?: object }): ToolInput {
    return listed.get(tool.name) ?? { tag: 'json-schema', val: JSON.stringify(tool.parameters ?? { type: 'object' }) };
}

// The model's arguments as the agent holds them: a JSON object as one (an
// empty one when the model's JSON does not parse, which the agent's own
// validation then reports), and a freeform call's text as FREEFORM_TEXT.
export function agentArguments(args?: Arguments): Record<string, JsonValue> {
    if (args?.tag === 'text') {
        return { [FREEFORM_TEXT]: args.val };
    }
    try {
        return JSON.parse(args?.val || '{}');
    } catch {
        return {};
    }
}

// Whether the agent's arguments object carries a freeform call's text.
function isFreeform(args: unknown): args is { [FREEFORM_TEXT]: string } {
    return (
        args != null &&
        typeof args === 'object' &&
        FREEFORM_TEXT in args &&
        typeof args[FREEFORM_TEXT] === 'string' &&
        Object.keys(args).length === 1
    );
}

// The agent's arguments as they leave it: a freeform call's text as `text`,
// verbatim, and anything else as `json`.
export function toolArguments(args: unknown): Arguments {
    return isFreeform(args)
        ? { tag: 'text', val: args[FREEFORM_TEXT] }
        : { tag: 'json', val: JSON.stringify(args ?? {}) };
}

// The arguments as a journal entry records them: a freeform call's text as a
// string, and an object as itself.
export function recordedArguments(args: unknown) {
    return isFreeform(args) ? args[FREEFORM_TEXT] : (args ?? {});
}
