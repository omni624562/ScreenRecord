# 閉眼偵測用的模型（從 Google MediaPipe 官方模型轉換）

相片分頁的「人物」用這兩個模型，在這台電腦上看相片裡的臉眼睛有沒有閉上，也順便擋掉不是臉的東西（動物、雜訊）。
程式會從這個資料夾下載（網址固定在某個 commit），下載後核對下面的大小與 SHA-256，每次載入前也再核對一次。

| 檔案 | 大小（bytes） | SHA-256 | 用途 |
|---|---|---|---|
| `face_landmarks_detector.onnx` | 4,920,997 | `cb6a76e380ff26a372957ebf69fbd523ce8a49266b882263868e1a333ed7d09a` | 臉部 478 個點、「是不是臉」的分數 |
| `face_blendshapes.onnx` | 1,883,548 | `baf1761013b93e3b4f55af456a80d74aa089a2fc377f5f8044ae48d0cc6a47f2` | 52 個表情分數（用其中的左眼、右眼眨眼） |

## 來源與授權

- 來源：Google MediaPipe 的 Face Landmarker 官方模型
  `https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task`
  （3,758,596 bytes，SHA-256 `64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff`）。
  這個檔案是 zip，裡面的兩個模型：
  - `face_landmarks_detector.tflite`（SHA-256 `c7d54204ce0448474c7f3fa9af494787c0965cbdd6f20fc72867e43046bd43d5`）
  - `face_blendshapes.tflite`（SHA-256 `4f36dded049db18d76048567439b2a7f58f1daabc00d78bfe8f3ad396a2d2082`）
- 授權：Apache License 2.0（見官方的 Model Card「MediaPipe Face Mesh V2」與「Blendshape V2」），全文在 `LICENSE-Apache-2.0.txt`。
- **修改**：只有把格式從 TFLite 轉成 ONNX（程式用的 tract 不支援官方檔案裡把 float16 權重轉回 float32 的運算），
  權重、網路結構都沒有改；轉換時 float16 的權重展開成 float32，所以檔案比較大。

## 怎麼轉的（可以自己重做一次，比對 SHA-256）

```bash
python -m venv venv
venv/bin/pip install tensorflow-cpu==2.21.0 tf2onnx==1.17.0
unzip face_landmarker.task face_landmarks_detector.tflite face_blendshapes.tflite
venv/bin/python -m tf2onnx.convert --tflite face_landmarks_detector.tflite --output face_landmarks_detector.onnx --opset 13
venv/bin/python -m tf2onnx.convert --tflite face_blendshapes.tflite --output face_blendshapes.onnx --opset 13
```

## 檢查過的事

- 轉換前後：同樣的輸入，TFLite 與 ONNX 的輸出差異最多 0.0005 像素（臉部的點）、0.0000013（表情分數）。
- tract（程式用的）與 ONNX Runtime 的輸出差異最多 0.0002 像素、0.0000015。
- 用到的運算只有一般的 Conv、PRelu、Add、MaxPool、Pad、Reshape、Transpose、Sigmoid、ReduceMean 等，沒有自訂運算。
- 和 MediaPipe 官方程式比對：同樣的臉部點算出的表情分數差異約 0.000001；我們自己裁臉（用 YuNet 找到的位置）
  得到的點和官方的平均差約 1% 臉寬，眨眼分數差在 0.1 以內。
- 眨眼分數：睜眼約 0～0.3，半閉、瞇眼約 0.5～0.7，全閉約 0.73～0.95；太小（約 30 像素）的臉笑起來也會到 0.5～0.8，
  所以程式只看夠大的臉。
