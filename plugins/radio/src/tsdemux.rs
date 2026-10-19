//! audio-only MPEG-TS 解复用：PAT → PMT（取首个 AAC 音频 PID）→ PES 拼接。
//!
//! 给 HLS 分片用（每段自带 PAT/PMT），容忍脏字节（逐字节重同步）与传输
//! 丢包（continuity_counter 断裂即丢弃半截 PES）。这是旧 gst
//! `--flags=audio` 纯音频收窄的内嵌版：视频 PID（实测蜻蜓 TS 另带一路
//! H.264）与非音频 PID 全部忽略。
//!
//! 只处理单包内完结的 PAT/PMT（audio-only 直播流的标准形态）；PES 按
//! PUSI 分段累积、下一 PUSI 或 [`TsDemux::flush`] 时输出。

/// ISO-13818 stream_type：AAC ADTS。
pub const ST_AAC_ADTS: u8 = 0x0F;
/// ISO-13818 stream_type：AAC LATM（fdk 用 TT_MP4_LATM_MCP1 解）。
pub const ST_AAC_LATM: u8 = 0x11;

const PKT: usize = 188;
/// 单个 PES 累积上限：64k电台一帧 ~400B，整段 PES 最大几 KB；512KB 是防失控值。
const PES_CAP: usize = 512 * 1024;

#[derive(Debug)]
pub struct TsDemux {
    pmt_pid: Option<u16>,
    audio_pid: Option<u16>,
    pub audio_type: Option<u8>,
    pes: Vec<u8>,
    /// 当前 PES 还差多少字节到声明长度（packet_length；usize::MAX = 未声明）。
    pes_need: usize,
    last_cc: Option<u8>,
    buf: Vec<u8>,
    /// 脏字节/丢包计数（诊断日志用）。
    pub glitches: u64,
}

impl TsDemux {
    pub fn new() -> Self {
        Self {
            pmt_pid: None,
            audio_pid: None,
            audio_type: None,
            pes: Vec::new(),
            pes_need: 0,
            last_cc: None,
            buf: Vec::new(),
            glitches: 0,
        }
    }

    /// 喂入任意分界的 TS 字节；解出的音频载荷（ADTS/LATM 裸流）追加到
    /// `out`。跨 `push` 保持 PES 与重同步状态（分片边界可能切开 PES）。
    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.buf.extend_from_slice(data);
        let mut pos = 0;
        while pos + PKT <= self.buf.len() {
            // 同步条件：当前 0x47，且（能看两包时）下一包边界也是 0x47
            let next_ok = pos + 2 * PKT > self.buf.len() || self.buf[pos + PKT] == 0x47;
            if self.buf[pos] != 0x47 || !next_ok {
                pos += 1;
                self.glitches += 1;
                continue;
            }
            let pkt: [u8; PKT] = self.buf[pos..pos + PKT].try_into().unwrap();
            self.packet(&pkt, out);
            pos += PKT;
        }
        self.buf.drain(..pos);
        // 长时间无同步的脏流：丢弃尾巴重新等同步
        if self.buf.len() > PKT * 64 {
            self.buf.clear();
            self.glitches += 1;
        }
    }

    fn packet(&mut self, p: &[u8], out: &mut Vec<u8>) {
        let pid = (((p[1] as u16) & 0x1F) << 8) | p[2] as u16;
        let pusi = p[1] & 0x40 != 0;
        let afc = (p[3] >> 4) & 0x3;
        let cc = p[3] & 0x0F;
        if afc & 0x1 == 0 {
            return; // 无载荷（保留值或纯适应字段）
        }
        let mut off = 4;
        if afc & 0x2 != 0 {
            off = 5 + p[4] as usize;
            if off >= PKT {
                return; // 适应字段长度异常
            }
        }
        let payload = &p[off..];

        if Some(pid) == self.audio_pid {
            self.audio_payload(pusi, cc, payload, out);
        } else if pid == 0 && pusi {
            self.parse_pat(payload);
        } else if self.audio_pid.is_none() && Some(pid) == self.pmt_pid && pusi {
            self.parse_pmt(payload);
        }
    }

    /// 跳过 pointer_field，返回 section 起始。
    fn section<'a>(payload: &'a [u8]) -> Option<&'a [u8]> {
        let pf = *payload.first()? as usize;
        payload.get(1 + pf..)
    }

    fn parse_pat(&mut self, payload: &[u8]) {
        let Some(sec) = Self::section(payload) else { return };
        if sec.len() < 12 || sec[0] != 0x00 {
            return;
        }
        // 表头 8 字节，其后每 4 字节一个 program；尾部 4 字节 CRC
        let mut i = 8;
        let end = sec.len() - 4;
        while i + 4 <= end {
            let program = ((sec[i] as u16) << 8) | sec[i + 1] as u16;
            let pid = (((sec[i + 2] as u16) & 0x1F) << 8) | sec[i + 3] as u16;
            if program != 0 {
                self.pmt_pid = Some(pid);
                return;
            }
            i += 4;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8]) {
        let Some(sec) = Self::section(payload) else { return };
        if sec.len() < 12 || sec[0] != 0x02 {
            return;
        }
        let pil = (((sec[10] as usize) & 0x0F) << 8) | sec[11] as usize;
        let mut i = 12 + pil;
        let end = sec.len() - 4;
        while i + 5 <= end {
            let st = sec[i];
            let epid = (((sec[i + 1] as u16) & 0x1F) << 8) | sec[i + 2] as u16;
            let esil = (((sec[i + 3] as usize) & 0x0F) << 8) | sec[i + 4] as usize;
            if st == ST_AAC_ADTS || st == ST_AAC_LATM {
                self.audio_pid = Some(epid);
                self.audio_type = Some(st);
                self.last_cc = None;
                return;
            }
            i += 5 + esil;
        }
    }

    fn audio_payload(&mut self, pusi: bool, cc: u8, payload: &[u8], out: &mut Vec<u8>) {
        // 连续性计数逐载荷包 +1；断裂即传输丢包，半截 PES 作废重来
        if let Some(expect) = self.last_cc {
            if cc != expect {
                self.glitches += 1;
                self.pes.clear();
            }
        }
        self.last_cc = Some(cc.wrapping_add(1) & 0x0F);

        if pusi {
            if !self.pes.is_empty() {
                out.append(&mut self.pes); // 上一个 PES 到此完结
            }
            if payload.len() >= 9
                && payload[0..3] == [0x00, 0x00, 0x01]
                && (0xC0..=0xDF).contains(&payload[3])
            {
                let hdr = payload[8] as usize;
                if let Some(body) = payload.get(9 + hdr..) {
                    // packet_length 从 flags 字段起算 = 3 + hdr_len + 载荷长；
                    // 按声明长度截取，TS 填充字节（0xFF）不入队——会被
                    // ADTS 解码器误认成同步字。
                    let pkt_len = ((payload[4] as usize) << 8) | payload[5] as usize;
                    self.pes_need = if pkt_len == 0 {
                        usize::MAX
                    } else {
                        pkt_len.saturating_sub(3 + hdr)
                    };
                    let take = body.len().min(self.pes_need);
                    self.pes.extend_from_slice(&body[..take]);
                    self.pes_need -= take;
                }
            } else {
                self.glitches += 1;
            }
        } else if !self.pes.is_empty() && self.pes_need > 0 {
            let take = payload.len().min(self.pes_need);
            if self.pes.len() + take > PES_CAP {
                self.pes.clear();
                self.pes_need = 0;
                self.glitches += 1;
            } else {
                self.pes.extend_from_slice(&payload[..take]);
                self.pes_need -= take;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 188B TS 包：无适应字段，载荷不足部分填 0xFF。
    fn pkt(pusi: bool, pid: u16, cc: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= 184);
        let mut p = vec![0x47];
        p.push(((pid >> 8) as u8 & 0x1F) | if pusi { 0x40 } else { 0 });
        p.push(pid as u8);
        p.push(0x10 | (cc & 0x0F)); // afc=1 纯载荷
        p.extend_from_slice(payload);
        p.resize(188, 0xFF);
        p
    }

    /// PAT：program 1 → `pmt_pid`（表体仿真实抓包形状）。
    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut s = vec![0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00];
        s.extend_from_slice(&0x0001u16.to_be_bytes());
        s.extend_from_slice(&((0xE000 | pmt_pid).to_be_bytes()));
        s.extend_from_slice(&[0x2E, 0x70, 0x19, 0x05]); // 伪 CRC
        let mut p = vec![0x00]; // pointer_field
        p.extend_from_slice(&s);
        pkt(true, 0, 0, &p)
    }

    /// PMT：一路 `stream_type`@`es_pid`（可另带一路 H.264 验证忽略）。
    fn pmt(entries: &[(u8, u16)]) -> Vec<u8> {
        let mut body = vec![0x02, 0xB0, 0x00, 0x00, 0x01, 0xC1, 0x00, 0x00, 0xE1, 0x00, 0xF0, 0x00];
        for (st, pid) in entries {
            body.push(*st);
            body.extend_from_slice(&((0xE000 | pid).to_be_bytes()));
            body.extend_from_slice(&[0xF0, 0x00]);
        }
        body[2] = (body.len() as u8 - 3 + 4) | 0xB0; // section_length 含 CRC
        body.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut p = vec![0x00];
        p.extend_from_slice(&body);
        pkt(true, 0x100, 0, &p)
    }

    /// 把 PES（带 9 字节头）切到 TS 包里：首包 PUSI，后续续包。
    fn pes_stream(pid: u16, body: &[u8], first_cc: u8) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xC0];
        pes.extend_from_slice(&((body.len() + 3) as u16).to_be_bytes()); // packet_length
        pes.extend_from_slice(&[0x00, 0x00, 0x00]); // flags1 flags2 hdr_len
        pes.extend_from_slice(body);
        let mut out = Vec::new();
        for (i, chunk) in pes.chunks(184).enumerate() {
            out.extend(pkt(i == 0, pid, first_cc + i as u8, chunk));
        }
        out
    }

    #[test]
    fn 全流程_pat_pmt_pes() {
        let mut d = TsDemux::new();
        let mut out = Vec::new();
        d.push(&pat(0x100), &mut out);
        d.push(&pmt(&[(ST_AAC_ADTS, 0x101), (0x1B, 0x102)]), &mut out);
        assert_eq!(d.audio_pid, Some(0x101)); // 取的是 AAC 而非 H.264
        let body: Vec<u8> = (0..400u32).map(|i| i as u8).collect();
        d.push(&pes_stream(0x101, &body, 0), &mut out);
        assert!(out.is_empty()); // PES 未完结不输出
        d.push(&pes_stream(0x101, &body, 3), &mut out); // 下一 PES 触发上一条输出（cc 接续）
        assert_eq!(out, body); // 恰好一个完整 PES，无填充
    }

    #[test]
    fn 脏字节前缀后重同步() {
        let mut d = TsDemux::new();
        let mut out = Vec::new();
        let garbage = vec![0x5Au8; 300]; // 非对齐垃圾
        d.push(&garbage, &mut out);
        d.push(&pat(0x100), &mut out);
        d.push(&pmt(&[(ST_AAC_ADTS, 0x101)]), &mut out);
        let body = vec![0xAAu8; 500];
        d.push(&pes_stream(0x101, &body, 0), &mut out);
        d.push(&pes_stream(0x101, &body, 3), &mut out);
        assert_eq!(&out[..body.len()], &body[..]);
        assert!(d.glitches > 0);
    }

    #[test]
    fn 丢包断裂丢弃半截_pes() {
        let mut d = TsDemux::new();
        let mut out = Vec::new();
        d.push(&pat(0x100), &mut out);
        d.push(&pmt(&[(ST_AAC_ADTS, 0x101)]), &mut out);
        let body = vec![0x77u8; 400]; // 需要 3 个 TS 包
        let mut stream = pes_stream(0x101, &body, 0);
        // 掐掉第二个包（模拟传输丢包）：cc 0 → 2 断裂
        stream.drain(188..376);
        d.push(&stream, &mut out);
        assert_eq!(d.glitches, 1);
        let good = pes_stream(0x101, &body, 3);
        d.push(&good, &mut out);
        let next = pes_stream(0x101, &body, 6);
        d.push(&next, &mut out);
        // 断裂的半截被丢弃，后续完整 PES 正常输出
        assert_eq!(&out[..body.len()], &body[..]);
    }

    #[test]
    fn 视频_pid与非音频忽略() {
        let mut d = TsDemux::new();
        let mut out = Vec::new();
        d.push(&pat(0x100), &mut out);
        d.push(&pmt(&[(0x1B, 0x102), (ST_AAC_ADTS, 0x101)]), &mut out);
        let body = vec![0x11u8; 100];
        d.push(&pes_stream(0x102, &body, 0), &mut out); // H.264 PID
        assert!(out.is_empty());
        d.push(&pes_stream(0x101, &body, 0), &mut out);
        d.push(&pes_stream(0x101, &body, 1), &mut out);
        assert_eq!(&out[..body.len()], &body[..]);
    }

}
