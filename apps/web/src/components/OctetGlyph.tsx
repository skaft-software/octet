import octetGlyphUrl from "../assets/octet-glyph.svg";

export function OctetGlyph({ className = "" }: { className?: string }) {
  return (
    <img
      className={`octet-glyph ${className}`.trim()}
      src={octetGlyphUrl}
      alt="octet"
    />
  );
}
