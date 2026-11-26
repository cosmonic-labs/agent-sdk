import type { AssistantMessage, Message } from '@earendil-works/pi-ai';

export type Reply = AssistantMessage & { declined?: 'refused' | 'filtered' };
export type TranscriptMessage = (Exclude<Message, AssistantMessage> | Reply) & { compaction?: boolean };
export interface Step {
    role: string;
    text: string;
    blocks: string[];
    tools: string[];
    run?: string;
    images?: string[];
}
export interface Counts {
    steps: number;
    input: number;
    output: number;
    toolCalls: number;
}
