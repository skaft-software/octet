// Pi 1.0.2 `ImageContent` is the only shape a handler ever sees for run
// attachments, and two native payloads carry them: the early `input` hook and
// the `before_agent_start` user event (`BeforeAgentStartUserEvent.images`, an
// optional member Pi omits when the run attached nothing). Both directions live
// here so neither path invents its own mapping: a handler that inspects or
// rewrites `event.images` gets the same Pi object graph whichever path named it.
import { invalid } from './errors.mjs';
import { canonicalToPi, piToCanonical } from './provider-context.mjs';

// The native input contract admits at most 256 attachments per prompt.
const MAX_IMAGES = 256;

// Pi `ImageContent[]` → canonical `Media[]` for a native transform result.
export function inputImages(images) {
  if (images === undefined) return undefined;
  if (!Array.isArray(images) || images.length > MAX_IMAGES) invalid('input images');
  const parts = piToCanonical([{ role: 'user', content: images }])[0].User.content;
  if (parts.some(part => !part.Media)) invalid('input images must contain images only');
  return parts.map(part => part.Media);
}

// Canonical `Media[]` → Pi `ImageContent[]` for an event handed to a handler.
// `undefined` is Pi's absent optional member: a text-only run attaches nothing,
// and the native payload reports that as an absent or null `images` member.
export function visibleImages(media, label = 'native input images') {
  if (media === undefined || media === null) return undefined;
  if (!Array.isArray(media) || media.length > MAX_IMAGES) invalid(label);
  return canonicalToPi([{ User: { content: media.map(Media => ({ Media })) } }])[0].content;
}
