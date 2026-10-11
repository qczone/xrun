import { useEffect, useRef, useState } from "react";
import { api, type Attachment } from "../../api";
import { Icon } from "../../components/Icon";
import { t } from "../../i18n";

// Serialize preview reads; retain only resized images in the activity list.
let previewQueue = Promise.resolve();
async function thumbnail(id: string) {
  const preview = await api.attachment(id);
  if (!preview.image || preview.attachment.status !== "available") return null;
  const image = new Image();
  image.src = preview.image;
  await image.decode();
  const scale = Math.min(1, 208 / image.width, 128 / image.height);
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(image.width * scale));
  canvas.height = Math.max(1, Math.round(image.height * scale));
  const context = canvas.getContext("2d");
  if (!context) return null;
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL("image/png");
}
export function ScreenshotThumbnail({
  attachment,
  active,
}: {
  attachment: Attachment;
  active: boolean;
}) {
  const container = useRef<HTMLSpanElement>(null);
  const [image, setImage] = useState<{ id: string; url: string } | null>(null);
  const available =
    attachment.status === "available" &&
    (attachment.expires_at_ms === null ||
      attachment.expires_at_ms > Date.now());
  useEffect(() => {
    if (
      !active ||
      !available ||
      image?.id === attachment.id ||
      !container.current ||
      !window.IntersectionObserver
    )
      return;
    let disposed = false;
    let requested = false;
    const observer = new IntersectionObserver((entries) => {
      if (requested || !entries.some((entry) => entry.isIntersecting)) return;
      requested = true;
      observer.disconnect();
      previewQueue = previewQueue.then(async () => {
        if (disposed) return;
        try {
          const value = await thumbnail(attachment.id);
          if (!disposed && value) setImage({ id: attachment.id, url: value });
        } catch {
          // Details retain the full preview, error, retry, and save actions.
        }
      });
    });
    observer.observe(container.current);
    return () => {
      disposed = true;
      observer.disconnect();
    };
  }, [active, available, attachment.id, image?.id]);
  return (
    <span className="screenshot-thumbnail" ref={container}>
      {available && image?.id === attachment.id ? (
        <img
          src={image.url}
          alt={t("activity.thumbnail", { name: attachment.name })}
        />
      ) : (
        <Icon name="image" />
      )}
    </span>
  );
}
