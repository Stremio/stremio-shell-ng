use crate::stremio_app::stremio_player::communication::{
    BoolProp, CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerProprChange, PropKey,
    PropVal,
};

use serde_test::{assert_tokens, Token};

#[test]
fn propr_change_tokens() {
    let prop = "test-prop";
    let tokens: [Token; 6] = [
        Token::Struct {
            name: "PlayerProprChange",
            len: 2,
        },
        Token::Str("name"),
        Token::None,
        Token::Str("data"),
        Token::None,
        Token::StructEnd,
    ];

    fn tokens_by_type(tokens: &[Token; 6], name: &'static str, val: mpv::Format, token: Token) {
        let mut typed_tokens = tokens.clone();
        typed_tokens[2] = Token::Str(name);
        typed_tokens[4] = token;
        assert_tokens(
            &PlayerProprChange::from_name_value(name.to_string(), val),
            &typed_tokens,
        );
    }
    tokens_by_type(&tokens, prop, mpv::Format::Flag(true), Token::Bool(true));
    tokens_by_type(&tokens, prop, mpv::Format::Int(1), Token::F64(1.0));
    tokens_by_type(&tokens, prop, mpv::Format::Double(1.0), Token::F64(1.0));
    tokens_by_type(&tokens, prop, mpv::Format::OsdStr("ok"), Token::Str("ok"));
    tokens_by_type(&tokens, prop, mpv::Format::Str("ok"), Token::Str("ok"));

    // JSON response
    tokens_by_type(
        &tokens,
        "track-list",
        mpv::Format::Str(r#""ok""#),
        Token::Str("ok"),
    );
    tokens_by_type(
        &tokens,
        "video-params",
        mpv::Format::Str(r#""ok""#),
        Token::Str("ok"),
    );
    tokens_by_type(
        &tokens,
        "metadata",
        mpv::Format::Str(r#""ok""#),
        Token::Str("ok"),
    );
}

#[test]
fn ended_tokens() {
    let tokens: [Token; 4] = [
        Token::Struct {
            name: "PlayerEnded",
            len: 1,
        },
        Token::Str("reason"),
        Token::None,
        Token::StructEnd,
    ];
    let mut typed_tokens = tokens.clone();
    typed_tokens[2] = Token::Str("error");
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv::EndFileReason::MPV_END_FILE_REASON_ERROR),
        &typed_tokens,
    );
    let mut typed_tokens = tokens.clone();
    typed_tokens[2] = Token::Str("quit");
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv::EndFileReason::MPV_END_FILE_REASON_QUIT),
        &typed_tokens,
    );
}

#[test]
fn ob_propr_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvObserveProp,
            InMsgArgs::ObProp(PropKey::Bool(BoolProp::Pause)),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-observe-prop"),
            Token::Str("pause"),
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn set_propr_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvSetProp,
            InMsgArgs::StProp(PropKey::Bool(BoolProp::Pause), PropVal::Bool(true)),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-set-prop"),
            Token::Tuple { len: 2 },
            Token::Str("pause"),
            Token::Bool(true),
            Token::TupleEnd,
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn command_stop_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvCommand,
            InMsgArgs::Cmd(CmdVal::Single((MpvCmd::Stop,))),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-command"),
            Token::Tuple { len: 1 },
            Token::Str("stop"),
            Token::TupleEnd,
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn command_loadfile_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvCommand,
            InMsgArgs::Cmd(CmdVal::Double(MpvCmd::Loadfile, "some_file".to_string())),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-command"),
            Token::Tuple { len: 2 },
            Token::Str("loadfile"),
            Token::Str("some_file"),
            Token::TupleEnd,
            Token::TupleStructEnd,
        ],
    );
}
