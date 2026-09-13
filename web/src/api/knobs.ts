/**
 * `GET /api/knobs` — the static `ft serve` schema.
 *
 * It cannot change without restarting the daemon (§4.2), so it is fetched once and
 * cached for the lifetime of the page. Keeping it out of the snapshot is what holds
 * the snapshot inside its 64 KiB budget.
 */

import { useEffect } from "react";
import type { Knob, KnobGroup, KnobSchema } from "./types.ts";
import { api } from "./client.ts";
import { Store, reportError, useStore } from "./store.ts";

export const knobStore = new Store<KnobSchema | null>(null);

let inFlight: Promise<KnobSchema> | null = null;

/** Fetch the schema once. Repeat calls share the first request, then the cache. */
export function loadKnobs(): Promise<KnobSchema> {
  const cached = knobStore.get();
  if (cached) return Promise.resolve(cached);
  if (inFlight) return inFlight;
  inFlight = api
    .knobs()
    .then((schema) => {
      knobStore.set(schema);
      inFlight = null;
      return schema;
    })
    .catch((error: unknown) => {
      inFlight = null;
      throw error;
    });
  return inFlight;
}

/** The schema, or null until it has arrived. */
export function useKnobs(): KnobSchema | null {
  const schema = useStore(knobStore);
  useEffect(() => {
    if (schema) return;
    loadKnobs().catch((error: unknown) => {
      reportError(error);
    });
  }, [schema]);
  return schema;
}

export function knobsInGroup(schema: KnobSchema | null, group: KnobGroup): Knob[] {
  if (!schema) return [];
  return schema.knobs.filter((knob) => knob.group === group);
}

export function knobByKey(schema: KnobSchema | null, key: string | null): Knob | null {
  if (!schema || !key) return null;
  return schema.knobs.find((knob) => knob.key === key) ?? null;
}

/**
 * The flags a knob excludes, as the "What it does" pane lists them: the schema's
 * own list, minus the knob itself, resolved to flag spellings.
 */
export function exclusions(schema: KnobSchema | null, knob: Knob): string[] {
  if (!schema) return [];
  return knob.exclusive_with
    .filter((key) => key !== knob.key)
    .map((key) => knobByKey(schema, key)?.flag ?? key);
}
