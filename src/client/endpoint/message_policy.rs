use crate::protocol::ServerMessage;

/// Effects that are meaningful only for the endpoint currently holding the host presentation
/// lease. During a frozen handoff they are intentionally dropped and the committed target asks
/// for one bounded replay, rather than allowing target graphics or modes onto the source frame.
pub(crate) fn is_presentation_effect(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::MouseCapture { .. }
            | ServerMessage::ClientShellKeyboardReportAll { .. }
            | ServerMessage::WindowTitle { .. }
            | ServerMessage::Graphics { .. }
            | ServerMessage::GraphicsFile { .. }
            | ServerMessage::GraphicsTransmissionRetired { .. }
            | ServerMessage::TerminalBell { .. }
    )
}

pub(crate) fn accepts_endpoint_message(
    endpoint_active: bool,
    activation_message: bool,
    command_response: bool,
    message: &ServerMessage,
) -> bool {
    endpoint_active
        || matches!(
            message,
            ServerMessage::EndpointControl { .. }
                | ServerMessage::SemanticNotification(_)
                | ServerMessage::ServerShutdown { .. }
                | ServerMessage::ClientShellSnapshot(_)
                | ServerMessage::Welcome { .. }
        )
        || (activation_message
            && matches!(
                message,
                // Serving is `shell.surface.v2` only; a v1 surface is not a live
                // activation lane, though the client still decodes its shape.
                ServerMessage::PaneSurfaceV2(_)
                    | ServerMessage::ClientShellEndpointResponseChunk { .. }
            ))
        || (command_response
            && matches!(
                message,
                ServerMessage::ClientShellEndpointResponseChunk { .. }
            ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_endpoints_deliver_metadata_but_not_presentation_effects() {
        assert!(accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::EndpointControl {
                kind: "snapshot".into(),
                data: "{}".into(),
            }
        ));
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::Clipboard {
                data: "text".into()
            }
        ));
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::WindowTitle {
                title: Some("remote".into())
            }
        ));
    }

    #[test]
    fn frozen_handoffs_drop_target_effects_until_the_post_commit_resync() {
        assert!(is_presentation_effect(&ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false,
        }));
        assert!(is_presentation_effect(&ServerMessage::GraphicsFile {
            path: "asset".into(),
            expected_len: 1,
            image_id: 1,
            transfer_id: 1,
            leading: Vec::new(),
            control: "a=t".into(),
            surface_asset: None,
        }));
        assert!(!is_presentation_effect(&ServerMessage::EndpointControl {
            kind: "shell.snapshot.v1".into(),
            data: "{}".into(),
        }));
    }

    #[test]
    fn pending_activation_accepts_only_its_surface_lane() {
        assert!(accepts_endpoint_message(
            false,
            true,
            false,
            &ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: "boot".into(),
                request_id: "surface".into(),
                final_chunk: true,
                data: Vec::new(),
            }
        ));
        assert!(!accepts_endpoint_message(
            false,
            true,
            false,
            &ServerMessage::MouseCapture {
                enabled: true,
                sgr_pixels: false,
            }
        ));
    }

    fn empty_frame() -> crate::protocol::PaneSurfaceFrame {
        crate::protocol::PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: crate::protocol::FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            collections: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn activation_accepts_only_the_current_surface_codec_and_an_inactive_endpoint_neither() {
        let v1 = ServerMessage::PaneSurface(empty_frame());
        let v2 = ServerMessage::PaneSurfaceV2(crate::protocol::PaneSurfaceFrameV2 {
            frame: empty_frame(),
            views: Vec::new(),
        });
        // Serving is v2 only: the v1 shape still decodes, but it is not a live
        // activation lane, and an inactive endpoint gets neither.
        assert!(accepts_endpoint_message(false, true, false, &v2));
        assert!(!accepts_endpoint_message(false, true, false, &v1));
        assert!(!accepts_endpoint_message(false, false, false, &v1));
        assert!(!accepts_endpoint_message(false, false, false, &v2));
    }
}
