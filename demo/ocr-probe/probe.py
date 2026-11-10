"""PP-OCRv4 链路探针：验证 det/cls/rec 三个 ONNX 与内嵌字符字典的配合。

只用于开发期确认模型输出形式与解码映射，不参与主程序构建。
运行：python demo/ocr-probe/probe.py（需 onnx / onnxruntime / opencv-python / pillow）
"""

import json
import os
import sys

import cv2
import numpy as np
import onnx
import onnxruntime as ort
from PIL import Image, ImageDraw, ImageFont

MODELS = os.environ.get("BEAD_OCR_MODELS", os.path.join(os.environ["TEMP"], "bead-ocr-probe"))


def load_dict(rec_path):
    model = onnx.load(rec_path, load_external_data=False)
    value = [p.value for p in model.metadata_props if p.key == "character"][0]
    return value.split("\n")


def make_image():
    width, height = 900, 220
    image = Image.new("RGB", (width, height), "white")
    draw = ImageDraw.Draw(image)
    font_cn = ImageFont.truetype(r"C:\Windows\Fonts\msyh.ttc", 40)
    draw.text((30, 20), "Bead 轻量翻译工具", font=font_cn, fill="black")
    draw.text((30, 100), "OCR 识别测试 12345", font=font_cn, fill="black")
    return np.array(image)[:, :, ::-1].copy()  # RGB -> BGR，和剪贴板 DIB 一致


def det(image, session):
    h, w = image.shape[:2]
    scale = min(960 / max(h, w), 1.0)
    nh, nw = max(int(round(h * scale / 32)) * 32, 32), max(int(round(w * scale / 32)) * 32, 32)
    resized = cv2.resize(image, (nw, nh))
    mean = np.array([0.485, 0.456, 0.406], dtype=np.float32) * 255
    std = np.array([0.229, 0.224, 0.225], dtype=np.float32) * 255
    data = (resized.astype(np.float32) - mean) / std
    data = data.transpose(2, 0, 1)[None]
    prob = session.run(None, {"x": data})[0][0, 0]
    return prob, (h, w, scale)


def ordered_points(points):
    """PaddleOCR get_mini_boxes：左上、右上、右下、左下。"""
    points = sorted(points.tolist(), key=lambda p: (p[0], p[1]))
    left = sorted(points[:2], key=lambda p: p[1])
    right = sorted(points[2:], key=lambda p: p[1])
    return np.array([left[0], right[0], right[1], left[1]], dtype=np.float32)


def boxes_from_prob(prob, meta):
    h, w, scale = meta
    mask = (prob > 0.3).astype(np.uint8) * 255
    contours, _ = cv2.findContours(mask, cv2.RETR_LIST, cv2.CHAIN_APPROX_SIMPLE)
    boxes = []
    for contour in contours:
        rect = cv2.minAreaRect(contour)
        (cx, cy), (bw, bh), angle = rect
        if min(bw, bh) < 3:
            continue
        if bw < bh:
            bw, bh = bh, bw
            angle += 90 if angle < 0 else -90
        area = bw * bh
        distance = area * 1.5 / (2 * (bw + bh) + 1e-6)
        rect = ((cx, cy), (bw + 2 * distance, bh + 2 * distance), angle)
        box = ordered_points(cv2.boxPoints(rect)) / scale
        boxes.append(box)
    boxes.sort(key=lambda b: (b[:, 1].min(), b[:, 0].min()))
    return boxes


def crop(image, box):
    src = np.array(box, dtype=np.float32)
    width = int(max(np.linalg.norm(src[0] - src[1]), np.linalg.norm(src[2] - src[3])))
    height = int(max(np.linalg.norm(src[0] - src[3]), np.linalg.norm(src[1] - src[2])))
    width, height = max(width, 1), max(height, 1)
    dst = np.array([[0, 0], [width, 0], [width, height], [0, height]], dtype=np.float32)
    transform = cv2.getPerspectiveTransform(src, dst)
    return cv2.warpPerspective(image, transform, (width, height))


def classify(images, session):
    if not images:
        return images
    batch = np.zeros((len(images), 3, 48, 192), dtype=np.float32)
    for i, img in enumerate(images):
        h, w = img.shape[:2]
        ratio = w / h
        width = min(int(48 * ratio), 192)
        resized = cv2.resize(img, (width, 48)).astype(np.float32)
        resized = (resized / 255.0 - 0.5) / 0.5
        batch[i, :, :, :width] = resized.transpose(2, 0, 1)
    out = session.run(None, {"x": batch})[0]
    result = []
    for img, logits in zip(images, out):
        if int(np.argmax(logits)) == 1 and float(np.max(logits)) > 0.9:
            img = cv2.rotate(img, cv2.ROTATE_180)
        result.append(img)
    return result


def recognize(images, session, dictionary):
    texts = []
    for img in images:
        h, w = img.shape[:2]
        ratio = w / h
        width = max(min(int(np.ceil(48 * ratio)), 3200), 4)
        resized = cv2.resize(img, (width, 48)).astype(np.float32)
        resized = (resized / 255.0 - 0.5) / 0.5
        data = resized.transpose(2, 0, 1)[None]
        out = session.run(None, {"x": data})[0][0]
        indices = out.argmax(axis=1)
        previous = 0
        chars = []
        for index in indices:
            index = int(index)
            if index != 0 and index != previous:
                chars.append(dictionary[index - 1] if index - 1 < len(dictionary) else " ")
            previous = index
        texts.append("".join(chars))
    return texts


def main():
    dictionary = load_dict(os.path.join(MODELS, "rec.onnx"))
    print("dict entries:", len(dictionary))
    image = make_image()
    det_session = ort.InferenceSession(os.path.join(MODELS, "det.onnx"))
    cls_session = ort.InferenceSession(os.path.join(MODELS, "cls.onnx"))
    rec_session = ort.InferenceSession(os.path.join(MODELS, "rec.onnx"))

    prob, meta = det(image, det_session)
    boxes = boxes_from_prob(prob, meta)
    crops = [crop(image, box) for box in boxes]
    crops = classify(crops, cls_session)
    texts = recognize(crops, rec_session, dictionary)
    print(json.dumps(texts, ensure_ascii=False, indent=2))
    cv2.imwrite(os.path.join(os.environ["TEMP"], "bead-ocr-probe", "input.png"), image)
    for index, box in enumerate(boxes):
        cv2.polylines(image, [np.int32(box)], True, (0, 0, 255), 2)
    cv2.imwrite(os.path.join(os.environ["TEMP"], "bead-ocr-probe", "boxes.png"), image)


if __name__ == "__main__":
    sys.exit(main())
